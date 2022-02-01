//! Ensures that `Pod`s are configured and running for each [`NifiCluster`]

use crate::config;
use crate::config::{
    build_authorizers_xml, build_bootstrap_conf, build_logback_xml, build_nifi_properties,
    build_state_management_xml, validated_product_config, NifiRepository, NIFI_BOOTSTRAP_CONF,
    NIFI_PROPERTIES, NIFI_STATE_MANAGEMENT_XML,
};
use rand::{distributions::Alphanumeric, Rng};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::authentication::get_auth_volumes;
use stackable_nifi_crd::NifiLogConfig;
use stackable_nifi_crd::{
    NifiCluster, NifiRole, HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME,
    PROTOCOL_PORT, PROTOCOL_PORT_NAME,
};
use stackable_nifi_crd::{APP_NAME, BALANCE_PORT, BALANCE_PORT_NAME};
use stackable_operator::client::Client;
use stackable_operator::k8s_openapi::api::core::v1::{
    Affinity, CSIVolumeSource, EmptyDirVolumeSource, EnvVar, EnvVarSource, Node, NodeAddress,
    ObjectFieldSelector, PodAffinityTerm, PodAntiAffinity, PodSpec, Probe, Secret,
    SecretVolumeSource, SecurityContext, TCPSocketAction, VolumeMount,
};
use stackable_operator::k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use stackable_operator::kube::runtime::reflector::ObjectRef;
use stackable_operator::kube::ResourceExt;
use stackable_operator::role_utils::RoleGroupRef;
use stackable_operator::{
    builder::{ConfigMapBuilder, ContainerBuilder, ObjectMetaBuilder, PodBuilder},
    k8s_openapi::{
        api::{
            apps::v1::{StatefulSet, StatefulSetSpec},
            core::v1::{
                ConfigMap, ConfigMapVolumeSource, PersistentVolumeClaim, PersistentVolumeClaimSpec,
                ResourceRequirements, Service, ServicePort, ServiceSpec, Volume,
            },
        },
        apimachinery::pkg::{api::resource::Quantity, apis::meta::v1::LabelSelector},
    },
    kube::{
        api::ObjectMeta,
        runtime::controller::{Context, ReconcilerAction},
    },
    labels::{role_group_selector_labels, role_selector_labels},
    product_config::{types::PropertyNameKind, ProductConfigManager},
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    time::Duration,
};

const FIELD_MANAGER_SCOPE: &str = "nificluster";

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub product_config: ProductConfigManager,
}

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object defines no version"))]
    ObjectHasNoVersion,
    #[snafu(display("object defines no name"))]
    ObjectHasNoName,
    #[snafu(display("object defines no spec"))]
    ObjectHasNoSpec,
    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,
    #[snafu(display("object defines no metastore role"))]
    NoNodeRole,
    #[snafu(display("failed to calculate global service name"))]
    GlobalServiceNameNotFound,
    #[snafu(display("failed to apply global Service"))]
    ApplyRoleService {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to check sensitive property key secret"))]
    SensitiveKeySecret {
        source: stackable_operator::error::Error,
    },
    #[snafu(display(
        "sensitive key secret [{}/{}] is missing, but auto generation is disabled",
        name,
        namespace
    ))]
    SensitiveKeySecretMissing { name: String, namespace: String },
    #[snafu(display("failed to apply Service for {}", rolegroup))]
    ApplyRoleGroupService {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<NifiCluster>,
    },
    #[snafu(display("failed to build ConfigMap for {}", rolegroup))]
    BuildRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<NifiCluster>,
    },
    #[snafu(display("failed to apply ConfigMap for {}", rolegroup))]
    ApplyRoleGroupConfig {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<NifiCluster>,
    },
    #[snafu(display("failed to apply StatefulSet for {}", rolegroup))]
    ApplyRoleGroupStatefulSet {
        source: stackable_operator::error::Error,
        rolegroup: RoleGroupRef<NifiCluster>,
    },
    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("Failed to get ZooKeeper connection string from config map {obj_ref}",))]
    GetZookeeperConnStringConfigMap {
        source: stackable_operator::error::Error,
        obj_ref: ObjectRef<ConfigMap>,
    },
    #[snafu(display("Failed to get ZooKeeper connection string from config map {obj_ref}",))]
    MissingZookeeperConnString { obj_ref: ObjectRef<ConfigMap> },
    #[snafu(display("Failed to load Product Config"))]
    ProductConfigLoadFailed { source: config::Error },
    #[snafu(display("Failed to find information about file [{}] in product config", kind))]
    ProductConfigKindNotSpecified { kind: String },
    #[snafu(display(
        "Failed to locate configmap [{}]. Please check if it is supplied correctly!",
        cm_name
    ))]
    KeyConfigMapNotFound { cm_name: String },
    #[snafu(display("Failed to find NiFi Service [{}]", name))]
    NifiServiceNotFound {
        source: stackable_operator::error::Error,
        name: String,
    },
    #[snafu(display("Failed to find any nodes in cluster {obj_ref} with selector {selector:?}",))]
    MissingNodes {
        source: stackable_operator::error::Error,
        obj_ref: ObjectRef<NifiCluster>,
        selector: LabelSelector,
    },
    #[snafu(display("Failed to find service {obj_ref}"))]
    MissingService {
        source: stackable_operator::error::Error,
        obj_ref: ObjectRef<Service>,
    },
    #[snafu(display("Failed to materialize authentication config element from k8s"))]
    MaterializeAuthConfig {
        source: stackable_nifi_crd::authentication::Error,
    },
    #[snafu(display("Failed to find an external port to use for proxy hosts"))]
    ExternalPort {},
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn reconcile_nifi(nifi: NifiCluster, ctx: Context<Ctx>) -> Result<ReconcilerAction> {
    tracing::info!("Starting reconcile");
    let client = &ctx.get_ref().client;
    let nifi_version = nifi_version(&nifi)?;
    let namespace = &nifi
        .metadata
        .namespace
        .clone()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?;

    // Zookeeper reference
    let zk_name = nifi.spec.zookeeper_reference.name.clone();
    // If no namespace is provided for the ZooKeeper reference, the same namespace as the NiFi
    // object is assumed
    let zk_namespace = nifi
        .spec
        .zookeeper_reference
        .namespace
        .clone()
        .unwrap_or_else(|| namespace.to_string());

    let zk_connect_string = client
        .get::<ConfigMap>(&zk_name, Some(&zk_namespace))
        .await
        .with_context(|_| GetZookeeperConnStringConfigMapSnafu {
            obj_ref: ObjectRef::new(&zk_name).within(&zk_namespace),
        })?
        .data
        .and_then(|mut data| data.remove("ZOOKEEPER"))
        .with_context(|| MissingZookeeperConnStringSnafu {
            obj_ref: ObjectRef::new(&zk_name).within(&zk_namespace),
        })?;

    tracing::info!("Checking for sensitive key configuration");
    check_or_generate_sensitive_key(client, &nifi).await?;

    let validated_config = validated_product_config(
        &nifi,
        nifi_version,
        nifi.spec.nodes.as_ref().context(NoNodeRoleSnafu)?,
        &ctx.get_ref().product_config,
    )
    .context(ProductConfigLoadFailedSnafu)?;

    let nifi_node_config = validated_config
        .get(&NifiRole::Node.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let node_role_service = build_node_role_service(&nifi)?;
    client
        .apply_patch(FIELD_MANAGER_SCOPE, &node_role_service, &node_role_service)
        .await
        .context(ApplyRoleServiceSnafu)?;

    let updated_role_service = client
        .get(&nifi.name(), nifi.namespace().as_deref())
        .await
        .with_context(|_| MissingServiceSnafu {
            obj_ref: ObjectRef::new(&nifi.name()).within(namespace),
        })?;

    for (rolegroup_name, rolegroup_config) in nifi_node_config.iter() {
        let rolegroup = nifi.node_rolegroup_ref(rolegroup_name);

        let rg_service = build_node_rolegroup_service(&nifi, &rolegroup)?;

        // This is due to the fact that users might access NiFi via these addresses, if they try to
        // connect from an external machine (not inside the k8s overlay network).
        // Since we cannot predict which of the addresses a user might decide to use we will simply
        // add all of them to the setting for now.
        // For more information see <https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#proxy_configuration>
        let proxy_hosts = get_proxy_hosts(client, &nifi, &updated_role_service).await?;

        let rg_configmap = build_node_rolegroup_config_map(
            client,
            &nifi,
            &rolegroup,
            &zk_connect_string,
            rolegroup_config,
            &proxy_hosts,
        )
        .await?;

        let rg_log_configmap = build_node_rolegroup_log_config_map(&nifi, &rolegroup)?;

        let rg_statefulset = build_node_rolegroup_statefulset(&nifi, &rolegroup, rolegroup_config)?;

        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_service, &rg_service)
            .await
            .with_context(|_| ApplyRoleGroupServiceSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_configmap, &rg_configmap)
            .await
            .with_context(|_| ApplyRoleGroupConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_log_configmap, &rg_log_configmap)
            .await
            .with_context(|_| ApplyRoleGroupConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?;

        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_statefulset, &rg_statefulset)
            .await
            .with_context(|_| ApplyRoleGroupStatefulSetSnafu {
                rolegroup: rolegroup.clone(),
            })?;
    }

    Ok(ReconcilerAction {
        requeue_after: None,
    })
}

/// The node-role service is the primary endpoint that should be used by clients that do not
/// perform internal load balancing including targets outside of the cluster.
pub fn build_node_role_service(nifi: &NifiCluster) -> Result<Service> {
    let role_name = NifiRole::Node.to_string();

    let role_svc_name = nifi
        .node_role_service_name()
        .context(GlobalServiceNameNotFoundSnafu)?;
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&role_svc_name)
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(nifi, APP_NAME, nifi_version(nifi)?, &role_name, "global")
            .build(),
        spec: Some(ServiceSpec {
            ports: Some(vec![ServicePort {
                name: Some(HTTPS_PORT_NAME.to_string()),
                port: HTTPS_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(role_selector_labels(nifi, APP_NAME, &role_name)),
            type_: Some("NodePort".to_string()),
            external_traffic_policy: Some("Local".to_string()),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

fn get_log_config(nifi: &NifiCluster, rolegroup: &RoleGroupRef<NifiCluster>) -> NifiLogConfig {
    let nodes = &nifi.spec.nodes.clone();
    let role_groups = &nodes.clone().unwrap().role_groups;

    match role_groups.get(&rolegroup.role_group.to_string()) {
        Some(role_group) => {
            let config = &role_group.config;
            config.config.clone().log.unwrap_or_default()
        }
        None => NifiLogConfig::default(),
    }
}

fn build_node_rolegroup_log_config_map(
    nifi: &NifiCluster,
    rolegroup: &RoleGroupRef<NifiCluster>,
) -> Result<ConfigMap> {
    ConfigMapBuilder::new()
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(nifi)
                .name(rolegroup.object_name() + "-log")
                .ownerreference_from_resource(nifi, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRefSnafu)?
                .with_recommended_labels(
                    nifi,
                    APP_NAME,
                    nifi_version(nifi)?,
                    &rolegroup.role,
                    &rolegroup.role_group,
                )
                .build(),
        )
        .add_data(
            "logback.xml",
            build_logback_xml(&get_log_config(nifi, rolegroup)),
        )
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup.clone(),
        })
}

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the administrator
async fn build_node_rolegroup_config_map(
    client: &Client,
    nifi: &NifiCluster,
    rolegroup: &RoleGroupRef<NifiCluster>,
    zk_connect_string: &str,
    config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    proxy_hosts: &str,
) -> Result<ConfigMap> {
    let namespace = &nifi
        .metadata
        .namespace
        .clone()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?;

    ConfigMapBuilder::new()
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(nifi)
                .name(rolegroup.object_name())
                .ownerreference_from_resource(nifi, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRefSnafu)?
                .with_recommended_labels(
                    nifi,
                    APP_NAME,
                    nifi_version(nifi)?,
                    &rolegroup.role,
                    &rolegroup.role_group,
                )
                .build(),
        )
        .add_data(
            NIFI_BOOTSTRAP_CONF,
            build_bootstrap_conf(
                config
                    .get(&PropertyNameKind::File(NIFI_BOOTSTRAP_CONF.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_BOOTSTRAP_CONF.to_string(),
                    })?
                    .clone(),
            ),
        )
        .add_data(
            NIFI_PROPERTIES,
            build_nifi_properties(
                &nifi.spec,
                zk_connect_string,
                proxy_hosts,
                config
                    .get(&PropertyNameKind::File(NIFI_PROPERTIES.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_PROPERTIES.to_string(),
                    })?
                    .clone(),
            ),
        )
        .add_data(
            NIFI_STATE_MANAGEMENT_XML,
            build_state_management_xml(&nifi.spec, zk_connect_string),
        )
        .add_data(
            "login-identity-providers.xml",
            stackable_nifi_crd::authentication::get_login_identity_provider_xml(
                client,
                &nifi.spec.authentication_config,
                namespace,
            )
            .await
            .context(MaterializeAuthConfigSnafu {})?,
        )
        .add_data("authorizers.xml", build_authorizers_xml())
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup.clone(),
        })
}

/// The rolegroup [`Service`] is a headless service that allows direct access to the instances of a certain rolegroup
///
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
fn build_node_rolegroup_service(
    nifi: &NifiCluster,
    rolegroup: &RoleGroupRef<NifiCluster>,
) -> Result<Service> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&rolegroup.object_name())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(
                nifi,
                APP_NAME,
                nifi_version(nifi)?,
                &rolegroup.role,
                &rolegroup.role_group,
            )
            .build(),
        spec: Some(ServiceSpec {
            cluster_ip: Some("None".to_string()),
            ports: Some(vec![
                ServicePort {
                    name: Some(HTTPS_PORT_NAME.to_string()),
                    port: HTTPS_PORT.into(),
                    protocol: Some("TCP".to_string()),
                    ..ServicePort::default()
                },
                ServicePort {
                    name: Some(METRICS_PORT_NAME.to_string()),
                    port: METRICS_PORT.into(),
                    protocol: Some("TCP".to_string()),
                    ..ServicePort::default()
                },
            ]),
            selector: Some(role_group_selector_labels(
                nifi,
                APP_NAME,
                &rolegroup.role,
                &rolegroup.role_group,
            )),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// The rolegroup [`StatefulSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`Service`] (from [`build_node_rolegroup_service`]).
fn build_node_rolegroup_statefulset(
    nifi: &NifiCluster,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
    config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
) -> Result<StatefulSet> {
    let mut container_builder = ContainerBuilder::new(APP_NAME);

    // get env vars and env overrides
    let mut env_vars: Vec<EnvVar> = config
        .get(&PropertyNameKind::Env)
        .with_context(|| ProductConfigKindNotSpecifiedSnafu {
            kind: "ENV".to_string(),
        })?
        .iter()
        .map(|(k, v)| EnvVar {
            name: k.clone(),
            value: Some(v.clone()),
            ..EnvVar::default()
        })
        .collect();

    // we need the POD_NAME env var to overwrite `nifi.cluster.node.address` later
    env_vars.push(EnvVar {
        name: "POD_NAME".to_string(),
        value_from: Some(EnvVarSource {
            field_ref: Some(ObjectFieldSelector {
                api_version: Some("v1".to_string()),
                field_path: "metadata.name".to_string(),
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    });

    let rolegroup = nifi
        .spec
        .nodes
        .as_ref()
        .context(NoNodeRoleSnafu)?
        .role_groups
        .get(&rolegroup_ref.role_group);

    let nifi_version = nifi_version(nifi)?;
    let image = format!(
        "docker.stackable.tech/stackable/nifi:{}-stackable0",
        nifi_version
    );

    let node_address = format!(
        "$POD_NAME.{}-node-{}.{}.svc.cluster.local",
        rolegroup_ref.cluster.name,
        rolegroup_ref.role_group,
        &nifi
            .metadata
            .namespace
            .as_ref()
            .with_context(|| ObjectHasNoNamespaceSnafu {})?
    );

    let sensitive_key_secret = &nifi.spec.sensitive_properties_config.key_secret;

    let auth_volumes = get_auth_volumes(&nifi.spec.authentication_config.method)
        .context(MaterializeAuthConfigSnafu)?;

    let mut container_prepare = ContainerBuilder::new("prepare")
        .image("docker.stackable.tech/soenkeliebau/tools:f18059a9")
        .command(vec!["/bin/bash".to_string(), "-c".to_string(), "-euo".to_string(), "pipefail".to_string()])
        .add_env_vars(env_vars.clone())
        .args(vec![[
            "echo Storing password",
            "echo secret > /stackable/keystore/password",
            "echo Cleaning up truststore - just in case",
            "rm -f /stackable/keystore/truststore.p12",
            "echo Creating truststore",
            "keytool -importcert -file /stackable/keystore/ca.crt -keystore /stackable/keystore/truststore.p12 -storetype pkcs12 -noprompt -alias ca_cert -storepass secret",
            "echo Creating certificate chain",
            "cat /stackable/keystore/ca.crt /stackable/keystore/tls.crt > /stackable/keystore/chain.crt",
            "echo Creating keystore",
            "openssl pkcs12 -export -in /stackable/keystore/chain.crt -inkey /stackable/keystore/tls.key -out /stackable/keystore/keystore.p12 --passout file:/stackable/keystore/password",
            "echo Cleaning up password",
            "rm -f /stackable/keystore/password",
            "echo Replacing config directory",
            "cp /conf/* /stackable/nifi/conf",
            "ln -s /stackable/logconfig/logback.xml /stackable/nifi/conf/logback.xml",
            "echo Replacing nifi.cluster.node.address in nifi.properties",
            &format!("sed -i \"s/nifi.cluster.node.address=/nifi.cluster.node.address={}/g\" /stackable/nifi/conf/nifi.properties", node_address),
            "echo Replacing nifi.web.https.host in nifi.properties",
            &format!("sed -i \"s/nifi.web.https.host=0.0.0.0/nifi.web.https.host={}/g\" /stackable/nifi/conf/nifi.properties", node_address),
            "echo Replacing nifi.sensitive.props.key in nifi.properties",
            "sed -i \"s|nifi.sensitive.props.key=|nifi.sensitive.props.key=$(cat /stackable/sensitiveproperty/nifiSensitivePropsKey)|g\" /stackable/nifi/conf/nifi.properties",
            "echo Replacing username and password in login-identity-provider.xml",
            "sed -i \"s|xxx|$(cat /stackable/adminuser/username)|g\" /stackable/nifi/conf/login-identity-providers.xml",
            "sed -i \"s|yyy|$(cat /stackable/adminuser/password | java -jar /bin/stackable-bcrypt.jar)|g\" /stackable/nifi/conf/login-identity-providers.xml",
            "echo chowning data directory",
            "chown -R stackable:stackable /stackable/data",
            "echo chmodding data directory",
            "chmod -R a=,u=rwX /stackable/data",
            "echo chowning keystore directory",
            "chown -R stackable:stackable /stackable/keystore",
            "echo chmodding keystore directory",
            "chmod -R a=,u=rwX /stackable/keystore",
        ]
        .join(" && ")])
        .add_volume_mount(
            &NifiRepository::Flowfile.repository(),
            &NifiRepository::Flowfile.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::Database.repository(),
            &NifiRepository::Database.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::Content.repository(),
            &NifiRepository::Content.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::Provenance.repository(),
            &NifiRepository::Provenance.mount_path(),
        ).add_volume_mount(
            &NifiRepository::State.repository(),
            &NifiRepository::State.mount_path(),
        )
        .add_volume_mount("conf", "/conf")
        .add_volume_mount("keystore", "/stackable/keystore")
        .add_volume_mount("activeconf", "/stackable/nifi/conf")
        .add_volume_mount("sensitiveproperty", "/stackable/sensitiveproperty")
        .build();

    for (name, (mount_path, _volume)) in &auth_volumes {
        container_prepare
            .volume_mounts
            .get_or_insert_with(Vec::default)
            .push(VolumeMount {
                mount_path: mount_path.to_string(),
                name: name.to_string(),
                ..VolumeMount::default()
            });
    }

    container_prepare
        .security_context
        .get_or_insert_with(SecurityContext::default)
        .run_as_user = Some(0);

    let mut container_nifi = container_builder
        .image(image)
        .command(vec!["/bin/bash".to_string(), "-c".to_string()])
        .args(vec![["bin/nifi.sh run"].join(" && ")])
        .add_env_vars(env_vars)
        .add_volume_mount("keystore", "/stackable/keystore")
        .add_volume_mount(
            &NifiRepository::Flowfile.repository(),
            &NifiRepository::Flowfile.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::Database.repository(),
            &NifiRepository::Database.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::Content.repository(),
            &NifiRepository::Content.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::Provenance.repository(),
            &NifiRepository::Provenance.mount_path(),
        )
        .add_volume_mount(
            &NifiRepository::State.repository(),
            &NifiRepository::State.mount_path(),
        )
        .add_volume_mount("activeconf", "/stackable/nifi/conf")
        .add_volume_mount("logconf", "/stackable/logconfig")
        .add_container_port(HTTPS_PORT_NAME, HTTPS_PORT.into())
        .add_container_port(PROTOCOL_PORT_NAME, PROTOCOL_PORT.into())
        .add_container_port(BALANCE_PORT_NAME, BALANCE_PORT.into())
        .add_container_port(METRICS_PORT_NAME, METRICS_PORT.into())
        .build();

    container_nifi.liveness_probe = Some(Probe {
        initial_delay_seconds: Some(10),
        period_seconds: Some(10),
        tcp_socket: Some(TCPSocketAction {
            port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
            ..TCPSocketAction::default()
        }),
        ..Probe::default()
    });
    container_nifi.startup_probe = Some(Probe {
        initial_delay_seconds: Some(10),
        period_seconds: Some(10),
        failure_threshold: Some(20 * 6),
        tcp_socket: Some(TCPSocketAction {
            port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
            ..TCPSocketAction::default()
        }),
        ..Probe::default()
    });

    let mut pod_template_builder = PodBuilder::new()
        .metadata_builder(|m| {
            m.with_recommended_labels(
                nifi,
                APP_NAME,
                nifi_version,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            )
        })
        .add_init_container(container_prepare)
        .add_container(container_nifi)
        // One volume for the NiFi configuration. A script will later on edit (e.g. nodename)
        // and copy the whole content to the <NIFI_HOME>/conf folder.
        .add_volume(Volume {
            name: "conf".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(rolegroup_ref.object_name()),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        })
        // The logback config is stored in a separate configmap, because this can be updated
        // on the fly without restarting NiFi
        // since the rest of the config is copied to a folder inside the container this would
        // not work for the log config, so this gets mounted from a separate configmap that can
        // be updated by the Kubelet
        .add_volume(Volume {
            name: "logconf".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(rolegroup_ref.object_name() + "-log"),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        })
        // One volume for the keystore and truststore data configmap
        .add_volume(Volume {
            name: "keystore".to_string(),
            csi: Some(CSIVolumeSource {
                driver: "secrets.stackable.tech".to_string(),
                volume_attributes: Some(get_stackable_secret_volume_attributes()),
                ..CSIVolumeSource::default()
            }),
            ..Volume::default()
        })
        .add_volume(Volume {
            name: "sensitiveproperty".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(sensitive_key_secret.to_string()),
                ..SecretVolumeSource::default()
            }),
            ..Volume::default()
        })
        .add_volume(Volume {
            empty_dir: Some(EmptyDirVolumeSource {
                medium: None,
                size_limit: None,
            }),
            name: "activeconf".to_string(),
            ..Volume::default()
        })
        .clone();

    for (_name, (_mount_path, volume)) in auth_volumes {
        pod_template_builder = pod_template_builder.add_volume(volume).clone();
    }

    let mut pod_template = pod_template_builder.build_template();

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/instance".to_string(),
        nifi.metadata
            .name
            .as_deref()
            .with_context(|| ObjectHasNoNameSnafu {})?
            .to_string(),
    );

    let anti_affinity = PodAntiAffinity {
        required_during_scheduling_ignored_during_execution: Some(vec![PodAffinityTerm {
            label_selector: Some(LabelSelector {
                match_expressions: None,
                match_labels: Some(labels),
            }),
            topology_key: "kubernetes.io/hostname".to_string(),
            ..PodAffinityTerm::default()
        }]),
        ..PodAntiAffinity::default()
    };

    let affinity = Affinity {
        pod_anti_affinity: Some(anti_affinity),
        ..Affinity::default()
    };

    pod_template
        .spec
        .get_or_insert_with(PodSpec::default)
        .affinity = Some(affinity);

    Ok(StatefulSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&rolegroup_ref.object_name())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(
                nifi,
                APP_NAME,
                nifi_version,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            )
            .build(),
        spec: Some(StatefulSetSpec {
            pod_management_policy: Some("Parallel".to_string()),
            replicas: if nifi.spec.stopped.unwrap_or(false) {
                Some(0)
            } else {
                rolegroup.and_then(|rg| rg.replicas).map(i32::from)
            },
            selector: LabelSelector {
                match_labels: Some(role_group_selector_labels(
                    nifi,
                    APP_NAME,
                    &rolegroup_ref.role,
                    &rolegroup_ref.role_group,
                )),
                ..LabelSelector::default()
            },
            service_name: rolegroup_ref.object_name(),
            template: pod_template,
            volume_claim_templates: Some(vec![
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Content.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Database.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Flowfile.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Provenance.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::State.repository(),
                    "1Gi",
                ),
            ]),
            ..StatefulSetSpec::default()
        }),
        status: None,
    })
}

async fn check_or_generate_sensitive_key(
    client: &Client,
    nifi: &NifiCluster,
) -> Result<bool, Error> {
    let sensitive_config = &nifi.spec.sensitive_properties_config;
    let namespace: &str = &nifi
        .metadata
        .namespace
        .clone()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?;

    match client
        .exists::<Secret>(&sensitive_config.key_secret, Some(namespace))
        .await
        .with_context(|_| SensitiveKeySecretSnafu {})?
    {
        true => Ok(false),
        false => {
            if !sensitive_config.auto_generate {
                return Err(Error::SensitiveKeySecretMissing {
                    name: sensitive_config.key_secret.clone(),
                    namespace: namespace.to_string(),
                });
            }
            tracing::info!("No existing sensitive properties key found, generating new one");
            let password: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(15)
                .map(char::from)
                .collect();

            let mut secret_data = BTreeMap::new();
            secret_data.insert("nifiSensitivePropsKey".to_string(), password);

            let new_secret = Secret {
                metadata: ObjectMetaBuilder::new()
                    .namespace(namespace)
                    .name(&sensitive_config.key_secret.to_string())
                    .build(),
                string_data: Some(secret_data),
                ..Secret::default()
            };
            client
                .create(&new_secret)
                .await
                .with_context(|_| SensitiveKeySecretSnafu {})?;
            Ok(true)
        }
    }
}

fn get_stackable_secret_volume_attributes() -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    result.insert(
        "secrets.stackable.tech/class".to_string(),
        "tls".to_string(),
    );
    result.insert(
        "secrets.stackable.tech/scope".to_string(),
        "node,pod".to_string(),
    );
    result
}

fn build_persistent_volume_claim_rwo_storage(name: &str, storage: &str) -> PersistentVolumeClaim {
    PersistentVolumeClaim {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            ..ObjectMeta::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            resources: Some(ResourceRequirements {
                requests: Some({
                    let mut map = BTreeMap::new();
                    map.insert("storage".to_string(), Quantity(storage.to_string()));
                    map
                }),
                ..ResourceRequirements::default()
            }),
            ..PersistentVolumeClaimSpec::default()
        }),
        ..PersistentVolumeClaim::default()
    }
}

fn external_node_port(nifi_service: &Service) -> Result<i32> {
    let external_ports = nifi_service
        .spec
        .as_ref()
        .with_context(|| ObjectHasNoSpecSnafu {})?
        .ports
        .as_ref()
        .with_context(|| ExternalPortSnafu {})?
        .iter()
        .filter(|p| p.name == Some(HTTPS_PORT_NAME.to_string()))
        .collect::<Vec<_>>();

    let port = external_ports
        .first()
        .with_context(|| ExternalPortSnafu {})?;

    port.node_port.with_context(|| ExternalPortSnafu {})
}

async fn get_proxy_hosts(
    client: &Client,
    nifi: &NifiCluster,
    nifi_service: &Service,
) -> Result<String> {
    let selector = LabelSelector {
        match_labels: {
            let mut labels = BTreeMap::new();
            labels.insert("kubernetes.io/os".to_string(), "linux".to_string());
            Some(labels)
        },
        ..LabelSelector::default()
    };

    let external_port = external_node_port(nifi_service)?;

    let cluster_nodes = client
        .list_with_label_selector::<Node>(None, &selector)
        .await
        .with_context(|_| MissingNodesSnafu {
            obj_ref: ObjectRef::from_obj(nifi),
            selector,
        })?;

    // We need the addresses of all nodes to add these to the NiFi proxy setting
    // Since there is no real convention about how to label these addresses we will simply
    // take all published addresses for now to be on the safe side.
    let mut proxy_setting = cluster_nodes
        .into_iter()
        .map(|node| {
            node.status
                .unwrap_or_default()
                .addresses
                .unwrap_or_default()
        })
        .flatten()
        .collect::<Vec<NodeAddress>>()
        .iter()
        .map(|node_address| format!("{}:{}", node_address.address, external_port))
        .collect::<Vec<_>>();

    // Also add the loadbalancer service
    proxy_setting.push(get_service_fqdn(nifi_service)?);

    Ok(proxy_setting.join(","))
}

fn get_service_fqdn(service: &Service) -> Result<String, Error> {
    let name = service
        .metadata
        .name
        .as_ref()
        .with_context(|| ObjectHasNoNameSnafu {})?
        .to_string();
    let namespace = service
        .metadata
        .namespace
        .as_ref()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?
        .to_string();
    Ok(format!("{}.{}.svc.cluster.local:8443", name, namespace))
}

pub fn nifi_version(nifi: &NifiCluster) -> Result<&str> {
    nifi.spec
        .version
        .as_deref()
        .context(ObjectHasNoVersionSnafu)
}

pub fn error_policy(_error: &Error, _ctx: Context<Ctx>) -> ReconcilerAction {
    ReconcilerAction {
        requeue_after: Some(Duration::from_secs(20)),
    }
}
