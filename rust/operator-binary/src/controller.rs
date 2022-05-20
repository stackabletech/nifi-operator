//! Ensures that `Pod`s are configured and running for each [`NifiCluster`]
use crate::config;
use crate::config::{
    build_authorizers_xml, build_bootstrap_conf, build_logback_xml, build_nifi_properties,
    build_state_management_xml, validated_product_config, NifiRepository, NIFI_BOOTSTRAP_CONF,
    NIFI_PROPERTIES, NIFI_STATE_MANAGEMENT_XML,
};
use rand::{distributions::Alphanumeric, Rng};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::{
    authentication::{get_auth_volumes, AUTH_VOLUME_MOUNT_PATH},
    NifiCluster, NifiConfig, NifiLogConfig, NifiRole, NifiStorageConfig, HTTPS_PORT,
    HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME, PROTOCOL_PORT, PROTOCOL_PORT_NAME,
};
use stackable_nifi_crd::{APP_NAME, BALANCE_PORT, BALANCE_PORT_NAME};
use stackable_operator::commons::resources::{NoRuntimeLimits, Resources};
use stackable_operator::config::merge::Merge;
use stackable_operator::k8s_openapi::api::apps::v1::StatefulSetUpdateStrategy;
use stackable_operator::role_utils::Role;
use stackable_operator::{
    builder::{ConfigMapBuilder, ContainerBuilder, ObjectMetaBuilder, PodBuilder},
    client::Client,
    k8s_openapi::{
        api::{
            apps::v1::{StatefulSet, StatefulSetSpec},
            batch::v1::{Job, JobSpec},
            core::v1::{
                Affinity, CSIVolumeSource, ConfigMap, ConfigMapKeySelector, ConfigMapVolumeSource,
                EmptyDirVolumeSource, EnvVar, EnvVarSource, Node, NodeAddress, ObjectFieldSelector,
                PodAffinityTerm, PodAntiAffinity, PodSpec, PodTemplateSpec, Probe, Secret,
                SecretVolumeSource, SecurityContext, Service, ServicePort, ServiceSpec,
                TCPSocketAction, Volume, VolumeMount,
            },
        },
        apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
    },
    kube::{
        runtime::controller::{Action, Context},
        runtime::reflector::ObjectRef,
        ResourceExt,
    },
    labels::{role_group_selector_labels, role_selector_labels},
    logging::controller::ReconcilerError,
    product_config::{types::PropertyNameKind, ProductConfigManager},
    role_utils::RoleGroupRef,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use strum::{EnumDiscriminants, IntoStaticStr};

const FIELD_MANAGER_SCOPE: &str = "nificluster";
const STACKABLE_TOOLS_IMAGE: &str = "docker.stackable.tech/stackable/tools:0.2.0-stackable0";

const KEYSTORE_VOLUME_NAME: &str = "keystore";
const KEYSTORE_NIFI_CONTAINER_MOUNT: &str = "/stackable/keystore";
const KEYSTORE_REPORTING_TASK_MOUNT: &str = "/stackable/cert";

pub struct Ctx {
    pub client: stackable_operator::client::Client,
    pub product_config: ProductConfigManager,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object defines no version"))]
    ObjectHasNoVersion,
    #[snafu(display("object defines no name"))]
    ObjectHasNoName,
    #[snafu(display("object defines no spec"))]
    ObjectHasNoSpec,
    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,
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
    #[snafu(display("object has no nodes defined"))]
    NoNodesDefined,
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
    #[snafu(display("failed to apply create ReportingTask job"))]
    ApplyCreateReportingTaskJob {
        source: stackable_operator::error::Error,
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
    ExternalPort,

    #[snafu(display("Could not build role service fqdn"))]
    NoRoleServiceFqdn,

    #[snafu(display("Bootstrap configuration error"))]
    BoostrapConfig { source: crate::config::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_nifi(nifi: Arc<NifiCluster>, ctx: Context<Ctx>) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let client = &ctx.get_ref().client;
    let nifi_version = nifi_version(&nifi)?;
    let namespace = &nifi
        .metadata
        .namespace
        .clone()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?;

    tracing::info!("Checking for sensitive key configuration");
    check_or_generate_sensitive_key(client, &nifi).await?;

    let validated_config = validated_product_config(
        &nifi,
        nifi_version,
        nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?,
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

        let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;

        let resource_definition = resolve_resource_config_for_rolegroup(&nifi, &rolegroup, role)?;

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
            rolegroup_config,
            &proxy_hosts,
            &resource_definition,
        )
        .await?;

        let rg_log_configmap = build_node_rolegroup_log_config_map(&nifi, &rolegroup)?;

        let rg_statefulset = build_node_rolegroup_statefulset(
            &nifi,
            &rolegroup,
            rolegroup_config,
            role,
            &resource_definition,
        )?;

        let reporting_task_job = build_reporting_task_job(&nifi, &rolegroup)?;

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

        client
            .apply_patch(
                FIELD_MANAGER_SCOPE,
                &reporting_task_job,
                &reporting_task_job,
            )
            .await
            .context(ApplyCreateReportingTaskJobSnafu)?;
    }

    Ok(Action::await_change())
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
    config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    proxy_hosts: &str,
    resource_definition: &Resources<NifiStorageConfig>,
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
                resource_definition.clone(),
                config
                    .get(&PropertyNameKind::File(NIFI_BOOTSTRAP_CONF.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_BOOTSTRAP_CONF.to_string(),
                    })?
                    .clone(),
            )
            .context(BoostrapConfigSnafu)?,
        )
        .add_data(
            NIFI_PROPERTIES,
            build_nifi_properties(
                &nifi.spec,
                proxy_hosts,
                config
                    .get(&PropertyNameKind::File(NIFI_PROPERTIES.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_PROPERTIES.to_string(),
                    })?
                    .clone(),
            ),
        )
        .add_data(NIFI_STATE_MANAGEMENT_XML, build_state_management_xml())
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
            .with_label("prometheus.io/scrape", "true")
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

fn resolve_resource_config_for_rolegroup(
    nifi: &NifiCluster,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
    role: &Role<NifiConfig>,
) -> Result<Resources<NifiStorageConfig, NoRuntimeLimits>> {
    // Initialize the result with all default values as baseline
    let conf_defaults = NifiConfig::default_resources();

    // Retrieve global role resource config
    let mut conf_role: Resources<NifiStorageConfig, NoRuntimeLimits> = nifi
        .spec
        .nodes
        .as_ref()
        .with_context(|| NoNodesDefinedSnafu {})?
        .config
        .config
        .resources
        .clone()
        .unwrap_or_default();

    // Retrieve rolegroup specific resource config
    let mut conf_rolegroup: Resources<NifiStorageConfig, NoRuntimeLimits> = role
        .role_groups
        .get(&rolegroup_ref.role_group)
        .and_then(|rg| rg.config.config.resources.clone())
        .unwrap_or_default();

    // Merge more specific configs into default config
    // Hierarchy is:
    // 1. RoleGroup
    // 2. Role
    // 3. Default
    conf_role.merge(&conf_defaults);
    conf_rolegroup.merge(&conf_role);

    Ok(conf_rolegroup)
}

/// The rolegroup [`StatefulSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`Service`] (from [`build_node_rolegroup_service`]).
fn build_node_rolegroup_statefulset(
    nifi: &NifiCluster,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
    config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    role: &Role<NifiConfig>,
    resource_definition: &Resources<NifiStorageConfig>,
) -> Result<StatefulSet> {
    let zookeeper_host = "ZOOKEEPER_HOSTS";
    let zookeeper_chroot = "ZOOKEEPER_CHROOT";

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

    env_vars.push(zookeeper_env_var(
        zookeeper_host,
        &nifi.spec.zookeeper_config_map_name,
    ));

    env_vars.push(zookeeper_env_var(
        zookeeper_chroot,
        &nifi.spec.zookeeper_config_map_name,
    ));

    let rolegroup = role.role_groups.get(&rolegroup_ref.role_group);

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
        .image(STACKABLE_TOOLS_IMAGE)
        .command(vec!["/bin/bash".to_string(), "-c".to_string(), "-euo".to_string(), "pipefail".to_string()])
        .add_env_vars(env_vars.clone())
        .args(vec![[
            "echo Storing password",
            &format!("echo secret > {keystore_path}/password", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Cleaning up truststore - just in case",
            &format!("rm -f {keystore_path}/truststore.p12", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Creating truststore",
            &format!("keytool -importcert -file {keystore_path}/ca.crt -keystore {keystore_path}/truststore.p12 -storetype pkcs12 -noprompt -alias ca_cert -storepass secret", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Creating certificate chain",
            &format!("cat {keystore_path}/ca.crt {keystore_path}/tls.crt > {keystore_path}/chain.crt", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Creating keystore",
            &format!("openssl pkcs12 -export -in {keystore_path}/chain.crt -inkey {keystore_path}/tls.key -out {keystore_path}/keystore.p12 --passout file:{keystore_path}/password", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Cleaning up password",
            &format!("rm -f {keystore_path}/password", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Replacing config directory",
            "cp /conf/* /stackable/nifi/conf",
            "ln -sf /stackable/logconfig/logback.xml /stackable/nifi/conf/logback.xml",
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
            &format!("chown -R stackable:stackable {keystore_path}", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo chmodding keystore directory",
            &format!("chmod -R a=,u=rwX {keystore_path}",keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
            "echo Replacing 'nifi.zookeeper.connect.string=xxxxxx' in /stackable/nifi/conf/nifi.properties",
            &format!("sed -i \"s|nifi.zookeeper.connect.string=xxxxxx|nifi.zookeeper.connect.string=${{{}}}|g\" /stackable/nifi/conf/nifi.properties", zookeeper_host),
            "echo Replacing 'nifi.zookeeper.root.node=xxxxxx' in /stackable/nifi/conf/nifi.properties",
            &format!("sed -i \"s|nifi.zookeeper.root.node=xxxxxx|nifi.zookeeper.root.node=${{{}}}|g\" /stackable/nifi/conf/nifi.properties", zookeeper_chroot),
            "echo Replacing connect string 'xxxxxx' in /stackable/nifi/conf/state-management.xml",
            &format!("sed -i \"s|xxxxxx|${{{}}}|g\" /stackable/nifi/conf/state-management.xml", zookeeper_host),
            "echo Replacing root node 'yyyyyy' in /stackable/nifi/conf/state-management.xml",
            &format!("sed -i \"s|yyyyyy|${{{}}}|g\" /stackable/nifi/conf/state-management.xml",zookeeper_chroot)
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
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
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
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
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

    container_nifi.resources = Some(resource_definition.clone().into());

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
        .add_volume(build_keystore_volume(KEYSTORE_VOLUME_NAME))
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
            update_strategy: Some(StatefulSetUpdateStrategy {
                type_: Some("OnDelete".to_string()),
                ..StatefulSetUpdateStrategy::default()
            }),
            volume_claim_templates: Some(vec![
                resource_definition.storage.content_repo.build_pvc(
                    &NifiRepository::Content.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                resource_definition.storage.database_repo.build_pvc(
                    &NifiRepository::Database.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                resource_definition.storage.flowfile_repo.build_pvc(
                    &NifiRepository::Flowfile.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                resource_definition.storage.provenance_repo.build_pvc(
                    &NifiRepository::Provenance.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                resource_definition.storage.state_repo.build_pvc(
                    &NifiRepository::State.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
            ]),
            ..StatefulSetSpec::default()
        }),
        status: None,
    })
}

/// Build the [`Job`](`stackable_operator::k8s_openapi::api::batch::v1::Job`) that creates a
/// NiFi `ReportingTask` in order to enable JVM and NiFi metrics.
///
/// The Job is run via the [`tools`](https://github.com/stackabletech/docker-images/tree/main/tools)
/// docker image and more specifically the `create_nifi_reporting_task.py` Python script.
///
/// This script uses the [`nipyapi`](https://nipyapi.readthedocs.io/en/latest/readme.html)
/// library to authenticate and run the required REST calls to the NiFi REST API.
///
/// In order to authenticate we need the `username` and `password` from the
/// [`NifiAuthenticationConfig`](`stackable_nifi_crd::authentication::NifiAuthenticationConfig`)
/// as well as a public certificate provided by the Stackable
/// [`secret-operator`](https://github.com/stackabletech/secret-operator)
///
fn build_reporting_task_job(
    nifi: &NifiCluster,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
) -> Result<Job> {
    let rolegroup_obj_name = rolegroup_ref.object_name();
    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;
    let nifi_connect_url = format!(
        "https://{rolegroup}-0.{rolegroup}.{namespace}.svc.cluster.local:{port}/nifi-api",
        rolegroup = rolegroup_obj_name,
        namespace = namespace,
        port = HTTPS_PORT
    );

    let mut container = ContainerBuilder::new("create-reporting-task")
        .image(STACKABLE_TOOLS_IMAGE)
        .command(vec!["sh".to_string(), "-c".to_string()])
        .args(vec![[
            "python/create_nifi_reporting_task.py",
            &format!("-n {}", nifi_connect_url),
            &format!("-u $(cat {}/username)", AUTH_VOLUME_MOUNT_PATH),
            &format!("-p $(cat {}/password)", AUTH_VOLUME_MOUNT_PATH),
            &format!("-v {}", nifi_version(nifi)?),
            &format!("-m {}", METRICS_PORT),
            &format!("-c {}/ca.crt", KEYSTORE_REPORTING_TASK_MOUNT),
        ]
        .join(" ")])
        // The VolumeMount for the secret operator key store certificates
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_REPORTING_TASK_MOUNT)
        .security_context(SecurityContext {
            run_as_user: Some(0),
            ..SecurityContext::default()
        })
        .build();

    // The Volume for the secret operator key store certificates
    let mut volumes = vec![build_keystore_volume(KEYSTORE_VOLUME_NAME)];

    // Volume and Volume mounts for the authentication secret
    let auth_volumes = get_auth_volumes(&nifi.spec.authentication_config.method)
        .context(MaterializeAuthConfigSnafu)?;

    for (name, (mount_path, volume)) in auth_volumes {
        container
            .volume_mounts
            .get_or_insert_with(Vec::default)
            .push(VolumeMount {
                mount_path: mount_path.to_string(),
                name: name.to_string(),
                ..VolumeMount::default()
            });
        volumes.push(volume);
    }

    let job_name = format!(
        "{}-create-reporting-task-{}",
        nifi.name(),
        nifi_version(nifi)?.replace('.', "-")
    );

    let pod = PodTemplateSpec {
        metadata: Some(
            ObjectMetaBuilder::new()
                .name(job_name.clone())
                .namespace_opt(nifi.namespace())
                .build(),
        ),
        spec: Some(PodSpec {
            containers: vec![container],
            // We use "OnFailure" here instead of "Never" to avoid spawning pods and pods. We just
            // restart the existing pod in case the script fails
            // (e.g. because the NiFi cluster is not ready yet).
            restart_policy: Some("OnFailure".to_string()),
            volumes: Some(volumes),
            ..Default::default()
        }),
    };

    let job = Job {
        metadata: ObjectMetaBuilder::new()
            .name(job_name)
            .namespace_opt(nifi.namespace())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .build(),
        spec: Some(JobSpec {
            backoff_limit: Some(100),
            ttl_seconds_after_finished: Some(120),
            template: pod,
            ..Default::default()
        }),
        status: None,
    };

    Ok(job)
}

async fn check_or_generate_sensitive_key(
    client: &Client,
    nifi: &NifiCluster,
) -> Result<bool, Error> {
    let sensitive_config = &nifi.spec.sensitive_properties_config;
    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;

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

fn build_keystore_volume(name: &str) -> Volume {
    Volume {
        name: name.to_string(),
        csi: Some(CSIVolumeSource {
            driver: "secrets.stackable.tech".to_string(),
            volume_attributes: Some(get_stackable_secret_volume_attributes()),
            ..CSIVolumeSource::default()
        }),
        ..Volume::default()
    }
}
/// Used for the `ZOOKEEPER_HOSTS` and `ZOOKEEPER_CHROOT` env vars.
fn zookeeper_env_var(name: &str, configmap_name: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value_from: Some(EnvVarSource {
            config_map_key_ref: Some(ConfigMapKeySelector {
                name: Some(configmap_name.to_string()),
                key: name.to_string(),
                ..ConfigMapKeySelector::default()
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    }
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
        .flat_map(|node| {
            node.status
                .unwrap_or_default()
                .addresses
                .unwrap_or_default()
        })
        .collect::<Vec<NodeAddress>>()
        .iter()
        .map(|node_address| format!("{}:{}", node_address.address, external_port))
        .collect::<Vec<_>>();

    // Also add the loadbalancer service
    proxy_setting.push(
        nifi.node_role_service_fqdn()
            .context(NoRoleServiceFqdnSnafu)?,
    );

    Ok(proxy_setting.join(","))
}

pub fn nifi_version(nifi: &NifiCluster) -> Result<&str> {
    nifi.spec
        .version
        .as_deref()
        .context(ObjectHasNoVersionSnafu)
}

pub fn error_policy(_error: &Error, _ctx: Context<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(10))
}
