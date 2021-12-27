//! Ensures that `Pod`s are configured and running for each [`NifiCluster`]

use crate::config::{
    build_bootstrap_conf, build_nifi_properties, build_state_management_xml, NIFI_BOOTSTRAP_CONF,
    NIFI_PROPERTIES, NIFI_STATE_MANAGEMENT_XML,
};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::{
    NifiCluster, NifiRole, HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME,
    PROTOCOL_PORT, PROTOCOL_PORT_NAME,
};
use stackable_nifi_crd::{APP_NAME, BALANCE_PORT, BALANCE_PORT_NAME};
use stackable_operator::k8s_openapi::api::core::v1::{EnvVar, EnvVarSource, ObjectFieldSelector};
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
    product_config_utils::{transform_all_roles_to_config, validate_all_roles_and_groups_config},
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    time::Duration,
};
use tracing::warn;

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
    #[snafu(display("failed to calculate service name for role {}", rolegroup))]
    RoleGroupServiceNameNotFound {
        rolegroup: RoleGroupRef<NifiCluster>,
    },
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
    #[snafu(display("invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to update status"))]
    ApplyStatus {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to parse db type {}", db_type))]
    InvalidDbType {
        source: strum::ParseError,
        db_type: String,
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
    #[snafu(display("Failed to transform configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::ConfigError,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn reconcile_nifi(nifi: NifiCluster, ctx: Context<Ctx>) -> Result<ReconcilerAction> {
    tracing::info!("Starting reconcile");
    let client = &ctx.get_ref().client;
    let nifi_version = nifi_version(&nifi)?;

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

    println!("Got zk connect string: {}", zk_connect_string);

    let validated_config = validate_all_roles_and_groups_config(
        nifi_version,
        &transform_all_roles_to_config(
            &nifi,
            [(
                NifiRole::Node.to_string(),
                (
                    vec![
                        PropertyNameKind::File(NIFI_BOOTSTRAP_CONF.to_string()),
                        PropertyNameKind::File(NIFI_PROPERTIES.to_string()),
                        PropertyNameKind::File(NIFI_STATE_MANAGEMENT_XML.to_string()),
                        PropertyNameKind::Env,
                    ],
                    nifi.spec.nodes.clone().context(NoNodeRole)?,
                ),
            )]
            .into(),
        )
        .with_context(|| ProductConfigTransform)?,
        &ctx.get_ref().product_config,
        false,
        false,
    )
    .context(InvalidProductConfig)?;

    let nifi_config = validated_config
        .get(&NifiRole::Node.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let node_role_service = build_node_role_service(&nifi)?;
    client
        .apply_patch(FIELD_MANAGER_SCOPE, &node_role_service, &node_role_service)
        .await
        .context(ApplyRoleService)?;

    for (rolegroup_name, rolegroup_config) in nifi_config.iter() {
        let rolegroup = nifi.node_rolegroup_ref(rolegroup_name);

        let rg_service = build_node_rolegroup_service(&nifi, &rolegroup)?;
        let rg_configmap = build_node_rolegroup_config_map(&nifi, &rolegroup, &zk_connect_string)?;
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

/// The server-role service is the primary endpoint that should be used by clients that do not
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
        .add_data(NIFI_BOOTSTRAP_CONF, build_bootstrap_conf())
        .add_data(
            NIFI_PROPERTIES,
            build_nifi_properties(&nifi.spec, zk_connect_string),
        )
        .add_data(
            NIFI_STATE_MANAGEMENT_XML,
            build_state_management_xml(&nifi.spec, zk_connect_string),
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
    metastore_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
) -> Result<StatefulSet> {
    let mut container_builder = ContainerBuilder::new(APP_NAME);

    for (property_name_kind, config) in metastore_config {
        match property_name_kind {
            PropertyNameKind::Env => {
                for (property_name, property_value) in config {
                    if property_name.is_empty() {
                        warn!("Received empty property_name for ENV... skipping");
                        continue;
                    }
                    container_builder.add_env_var(property_name, property_value);
                }
            }
            PropertyNameKind::Cli => {}
            _ => {}
        }
    }

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

    let container_nifi = container_builder
        .image(image)
        .command(vec!["/bin/bash".to_string(), "-c".to_string()])
        .args(vec![
            format!(
                "/stackable/bin/copy_assets /conf;
                 /stackable/bin/update_config;
                 sed -i \"s/nifi.web.https.host=/nifi.web.https.host=$POD_NAME.simple-node-default.default.svc.cluster.local/g\" /stackable/nifi/conf/nifi.properties;
                 sed -i \"s/nifi.cluster.node.address=/nifi.cluster.node.address=$POD_NAME.simple-node-default.default.svc.cluster.local/g\" /stackable/nifi/conf/nifi.properties;
                 bin/nifi.sh run"
            )
        ])
        .add_env_vars(vec![EnvVar {
            name: "POD_NAME".to_string(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    api_version: Some("v1".to_string()),
                    field_path: "metadata.name".to_string(),
                }),
                ..EnvVarSource::default()
            }),
            ..EnvVar::default()
        }])
        .add_volume_mount("conf", "conf")
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
                .add_container(container_nifi)
                .add_volume(Volume {
                    name: "conf".to_string(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: Some(rolegroup_ref.object_name()),
                        ..ConfigMapVolumeSource::default()
                    }),
                    ..Volume::default()
                })
                .build_template(),
            volume_claim_templates: Some(vec![PersistentVolumeClaim {
                metadata: ObjectMeta {
                    name: Some("data".to_string()),
                    ..ObjectMeta::default()
                },
                spec: Some(PersistentVolumeClaimSpec {
                    access_modes: Some(vec!["ReadWriteOnce".to_string()]),
                    resources: Some(ResourceRequirements {
                        requests: Some({
                            let mut map = BTreeMap::new();
                            map.insert("storage".to_string(), Quantity("1Gi".to_string()));
                            map
                        }),
                        ..ResourceRequirements::default()
                    }),
                    ..PersistentVolumeClaimSpec::default()
                }),
                ..PersistentVolumeClaim::default()
            }]),
            ..StatefulSetSpec::default()
        }),
        status: None,
    })
}

pub fn nifi_version(nifi: &NifiCluster) -> Result<&str> {
    nifi.spec.version.as_deref().context(ObjectHasNoVersion)
}

pub fn error_policy(_error: &Error, _ctx: Context<Ctx>) -> ReconcilerAction {
    ReconcilerAction {
        requeue_after: Some(Duration::from_secs(5)),
    }
}
