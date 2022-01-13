//! Ensures that `Pod`s are configured and running for each [`NifiCluster`]

use crate::config;
use crate::config::{
    build_bootstrap_conf, build_nifi_properties, build_state_management_xml,
    validated_product_config, NifiRepository, NIFI_BOOTSTRAP_CONF, NIFI_PROPERTIES,
    NIFI_STATE_MANAGEMENT_XML,
};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::authentication;
use stackable_nifi_crd::authentication::NifiAuthenticationMethodConfig;
use stackable_nifi_crd::{
    NifiCluster, NifiRole, HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME,
    PROTOCOL_PORT, PROTOCOL_PORT_NAME,
};
use stackable_nifi_crd::{APP_NAME, BALANCE_PORT, BALANCE_PORT_NAME};
use stackable_operator::client::Client;
use stackable_operator::k8s_openapi::api::core::v1::{
    CSIVolumeSource, EnvVar, EnvVarSource, Node, ObjectFieldSelector, SecretVolumeSource,
    SecurityContext,
};
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
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object defines no version"))]
    ObjectHasNoVersion,
    #[snafu(display("object defines no metastore role"))]
    NoNodeRole,
    #[snafu(display("failed to calculate global service name"))]
    GlobalServiceNameNotFound,
    #[snafu(display("failed to apply global Service"))]
    ApplyRoleService {
        source: stackable_operator::error::Error,
    },
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
    #[snafu(display(
        "Failed to get ZooKeeper connection string from config map {} in namespace {}",
        cm_name,
        namespace
    ))]
    GetZookeeperConnStringConfigMap {
        source: stackable_operator::error::Error,
        cm_name: String,
        namespace: String,
    },
    #[snafu(display(
        "Failed to get ZooKeeper connection string from config map {} in namespace {}",
        cm_name,
        namespace
    ))]
    MissingZookeeperConnString { cm_name: String, namespace: String },
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
    #[snafu(display(
        "Failed to find any nodes in namespace [{}] with selector [{:?}]",
        namespace,
        selector
    ))]
    MissingNodes {
        source: stackable_operator::error::Error,
        namespace: String,
        selector: LabelSelector,
    },
    #[snafu(display("Failed to find service [{}/{}]", name, namespace))]
    MissingService {
        source: stackable_operator::error::Error,
        name: String,
        namespace: String,
    },
    #[snafu(display("Failed to materialize authentication config element from k8s"))]
    MaterializeError {
        source: stackable_nifi_crd::authentication::Error,
    },
    #[snafu(display(
        "Failed to obtain name of secret that contains the sensitive-information-key: [{}]",
        message
    ))]
    SensitiveKeySecretError { message: String },
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn reconcile_nifi(nifi: NifiCluster, ctx: Context<Ctx>) -> Result<ReconcilerAction> {
    tracing::info!("Starting reconcile");
    let client = &ctx.get_ref().client;
    let nifi_version = nifi_version(&nifi)?;

    // Zookeeper reference
    let zk_name = nifi.spec.zookeeper_reference.name.clone();
    let zk_namespace = nifi.spec.zookeeper_reference.namespace.clone();
    let zk_connect_string = client
        .get::<ConfigMap>(&zk_name, Some(&zk_namespace))
        .await
        .with_context(|| GetZookeeperConnStringConfigMap {
            cm_name: zk_name.to_string(),
            namespace: zk_namespace.to_string(),
        })?
        .data
        .and_then(|mut data| data.remove("ZOOKEEPER"))
        .with_context(|| MissingZookeeperConnString {
            cm_name: zk_name.to_string(),
            namespace: zk_namespace.to_string(),
        })?;

    // read authentication
    let auth_config = authentication::materialize_auth_config(client, &nifi.spec.authentication_config)
        .await
        .with_context(|| MaterializeError {})?;

    println!("auth config: [{:?}]", auth_config);

    let validated_config = validated_product_config(
        &nifi,
        nifi_version,
        nifi.spec.nodes.as_ref().context(NoNodeRole)?,
        &ctx.get_ref().product_config,
    )
    .context(ProductConfigLoadFailed)?;

    let nifi_node_config = validated_config
        .get(&NifiRole::Node.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let node_role_service = build_node_role_service(&nifi)?;
    client
        .apply_patch(FIELD_MANAGER_SCOPE, &node_role_service, &node_role_service)
        .await
        .context(ApplyRoleService)?;

    let updated_role_service = client
        .get(&nifi.name(), nifi.namespace().as_deref())
        .await
        .with_context(|| MissingService {
            name: nifi.name(),
            namespace: nifi.namespace().unwrap_or_default(),
        })?;

    for (rolegroup_name, rolegroup_config) in nifi_node_config.iter() {
        let rolegroup = nifi.node_rolegroup_ref(rolegroup_name);

        let rg_service = build_node_rolegroup_service(&nifi, &rolegroup)?;

        // node addresses
        let proxy_hosts = node_addresses(client, &nifi, &updated_role_service).await?;

        let rg_configmap = build_node_rolegroup_config_map(
            &nifi,
            &rolegroup,
            &zk_connect_string,
            rolegroup_config,
            &proxy_hosts,
            &auth_config,
        )?;

        let rg_statefulset = build_node_rolegroup_statefulset(&nifi, &rolegroup, rolegroup_config)?;

        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_service, &rg_service)
            .await
            .with_context(|| ApplyRoleGroupService {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_configmap, &rg_configmap)
            .await
            .with_context(|| ApplyRoleGroupConfig {
                rolegroup: rolegroup.clone(),
            })?;
        client
            .apply_patch(FIELD_MANAGER_SCOPE, &rg_statefulset, &rg_statefulset)
            .await
            .with_context(|| ApplyRoleGroupStatefulSet {
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
        .context(GlobalServiceNameNotFound)?;
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&role_svc_name)
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRef)?
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

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the administrator
fn build_node_rolegroup_config_map(
    nifi: &NifiCluster,
    rolegroup: &RoleGroupRef<NifiCluster>,
    zk_connect_string: &str,
    config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    proxy_hosts: &str,
    authorizer_config: &NifiAuthenticationMethodConfig,
) -> Result<ConfigMap> {
    ConfigMapBuilder::new()
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(nifi)
                .name(rolegroup.object_name())
                .ownerreference_from_resource(nifi, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRef)?
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
                    .with_context(|| ProductConfigKindNotSpecified {
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
                    .with_context(|| ProductConfigKindNotSpecified {
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
            stackable_nifi_crd::authentication::get_authorizer_xml(authorizer_config),
        )
        .build()
        .with_context(|| BuildRoleGroupConfig {
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
            .context(ObjectMissingMetadataForOwnerRef)?
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
/// corresponding [`Service`] (from [`build_rolegroup_service`]).
fn build_node_rolegroup_statefulset(
    nifi: &NifiCluster,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
    config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
) -> Result<StatefulSet> {
    let mut container_builder = ContainerBuilder::new(APP_NAME);

    // get env vars and env overrides
    let mut env_vars: Vec<EnvVar> = config
        .get(&PropertyNameKind::Env)
        .with_context(|| ProductConfigKindNotSpecified {
            kind: "ENV".to_string(),
        })?
        .iter()
        .map(|(k, v)| EnvVar {
            name: k.clone(),
            value: Some(v.clone()),
            ..EnvVar::default()
        })
        .collect();

    let sensitive_property_key: Vec<Option<String>> = env_vars
        .iter()
        .filter(|var| &var.name == "NIFI_SENSITIVE_PROPS_KEY")
        .map(|var| var.value.to_owned())
        .collect();

    let key = sensitive_property_key
        .first()
        .with_context(|| SensitiveKeySecretError {
            message: "Key was not present in environment variable".to_string(),
        })?
        .clone()
        .with_context(|| SensitiveKeySecretError {
            message: "No value set for key in environment".to_string(),
        })?;

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
        .context(NoNodeRole)?
        .role_groups
        .get(&rolegroup_ref.role_group);

    let nifi_version = nifi_version(nifi)?;
    let image = format!(
        "docker.stackable.tech/stackable/nifi:{}-stackable0",
        nifi_version
    );

    let node_address = format!(
        "$POD_NAME.{}-node-{}.default.svc.cluster.local",
        rolegroup_ref.cluster.name, rolegroup_ref.role_group
    );

    let mut container_prepare = ContainerBuilder::new("prepare")
        .image(&image)
        .command(vec!["/bin/bash".to_string(), "-c".to_string()])
        .args(vec![[
            "microdnf install openssl",
            "echo Storing password",
            "echo secret > /stackable/keystore/password",
            "echo Creating truststore",
            "keytool -importcert -file /stackable/keystore/ca.crt -keystore /stackable/keystore/truststore.p12 -storetype pkcs12 -noprompt -alias ca_cert -storepass secret",
            "echo Creating certificate chain",
            "cat /stackable/keystore/ca.crt /stackable/keystore/tls.crt > /stackable/keystore/chain.crt",
            "echo Creating keystore",
            "openssl pkcs12 -export -in /stackable/keystore/chain.crt -inkey /stackable/keystore/tls.key -out /stackable/keystore/keystore.p12 --passout file:/stackable/keystore/password",
            "echo Cleaning up password",
            "rm -f /stackable/keystore/password",
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
        )
        .add_volume_mount("keystore", "/stackable/keystore")
        .build();

    container_prepare
        .security_context
        .get_or_insert_with(SecurityContext::default)
        .run_as_user = Some(0);

    let container_nifi = container_builder
        .image(image)
        .command(vec!["/bin/bash".to_string(), "-c".to_string()])
        .args(vec![
            [
                "/stackable/bin/copy_assets /conf",
                "echo Replacing nifi.cluster.node.address in nifi.properties",
                &format!("sed -i \"s/nifi.cluster.node.address=/nifi.cluster.node.address={}/g\" /stackable/nifi/conf/nifi.properties", node_address),
                "echo Replacing nifi.web.https.host in nifi.properties",
                &format!("sed -i \"s/nifi.web.https.host=0.0.0.0/nifi.web.https.host={}/g\" /stackable/nifi/conf/nifi.properties", node_address),
                "echo Replacing nifi.sensitive.props.key in nifi.properties",
                "sed -i \"s|nifi.sensitive.props.key=|nifi.sensitive.props.key=$(cat /stackable/sensitiveproperty/nifiSensitivePropsKey)|g\" /stackable/nifi/conf/nifi.properties",
                "bin/nifi.sh run",
            ]
            .join(" && "),
        ])
        .add_env_vars(env_vars)
        .add_volume_mount("conf", "conf")
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
        .add_volume_mount("sensitiveproperty", "/stackable/sensitiveproperty")
        .add_container_port(HTTPS_PORT_NAME, HTTPS_PORT.into())
        .add_container_port(PROTOCOL_PORT_NAME, PROTOCOL_PORT.into())
        .add_container_port(BALANCE_PORT_NAME, BALANCE_PORT.into())
        .add_container_port(METRICS_PORT_NAME, METRICS_PORT.into())
        .build();

    Ok(StatefulSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&rolegroup_ref.object_name())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRef)?
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
            template: PodBuilder::new()
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
                        secret_name: Some(key),
                        ..SecretVolumeSource::default()
                    }),
                    ..Volume::default()
                })
                .build_template(),
            volume_claim_templates: Some(vec![
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Flowfile.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Database.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Content.repository(),
                    "2Gi",
                ),
                build_persistent_volume_claim_rwo_storage(
                    &NifiRepository::Provenance.repository(),
                    "2Gi",
                ),
            ]),
            ..StatefulSetSpec::default()
        }),
        status: None,
    })
}

fn get_stackable_secret_volume_attributes() -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    result.insert(
        "secrets.stackable.tech/type".to_string(),
        "secret".to_string(),
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

async fn external_node_port(nifi_service: &Service) -> Result<i32> {
    Ok(nifi_service
        .spec
        .as_ref()
        .unwrap()
        .ports
        .as_ref()
        .unwrap()
        .iter()
        .filter(|p| p.name == Some(HTTPS_PORT_NAME.to_string()))
        .map(|p| p.node_port.unwrap())
        .collect::<Vec<_>>()
        .pop()
        .unwrap())
}

async fn node_addresses(
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

    let external_port = external_node_port(nifi_service).await?;

    let cluster_nodes = client
        .list_with_label_selector::<Node>(None, &selector)
        .await
        .with_context(|| MissingNodes {
            namespace: nifi.metadata.namespace.as_deref().unwrap().to_string(),
            selector,
        })?;

    Ok(cluster_nodes
        .into_iter()
        .map(|node| node.status.unwrap().addresses.unwrap())
        .flatten()
        .filter(|address| address.type_ == *"ExternalIP")
        .map(|address| address.address)
        .collect::<Vec<_>>()
        .iter()
        .map(|node_ip| format!("{}:{}", node_ip, external_port))
        .collect::<Vec<_>>()
        .join(","))
}

pub fn nifi_version(nifi: &NifiCluster) -> Result<&str> {
    nifi.spec.version.as_deref().context(ObjectHasNoVersion)
}

pub fn error_policy(_error: &Error, _ctx: Context<Ctx>) -> ReconcilerAction {
    ReconcilerAction {
        requeue_after: Some(Duration::from_secs(20)),
    }
}
