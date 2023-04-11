//! Ensures that `Pod`s are configured and running for each [`NifiCluster`]
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    ops::Deref,
    sync::Arc,
    time::Duration,
};

use rand::{distributions::Alphanumeric, Rng};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{
        ConfigMapBuilder, ContainerBuilder, ObjectMetaBuilder, PodBuilder,
        PodSecurityContextBuilder, VolumeBuilder,
    },
    client::Client,
    cluster_resources::{ClusterResourceApplyStrategy, ClusterResources},
    commons::product_image_selection::ResolvedProductImage,
    config::fragment,
    k8s_openapi::{
        api::{
            apps::v1::{StatefulSet, StatefulSetSpec, StatefulSetUpdateStrategy},
            batch::v1::{Job, JobSpec},
            core::v1::{
                CSIVolumeSource, ConfigMap, ConfigMapKeySelector, ConfigMapVolumeSource,
                EmptyDirVolumeSource, EnvVar, EnvVarSource, Node, ObjectFieldSelector,
                PodSecurityContext, Probe, Secret, SecretVolumeSource, Service, ServicePort,
                ServiceSpec, TCPSocketAction, Volume,
            },
        },
        apimachinery::pkg::{
            api::resource::Quantity, apis::meta::v1::LabelSelector, util::intstr::IntOrString,
        },
    },
    kube::{runtime::controller::Action, runtime::reflector::ObjectRef, Resource, ResourceExt},
    labels::{role_group_selector_labels, role_selector_labels, ObjectLabels},
    logging::controller::ReconcilerError,
    product_config::{types::PropertyNameKind, ProductConfigManager},
    product_logging::{
        self,
        spec::{
            ConfigMapLogConfig, ContainerLogConfig, ContainerLogConfigChoice,
            CustomContainerLogConfig,
        },
    },
    role_utils::{Role, RoleGroupRef},
    status::condition::{
        compute_conditions, operations::ClusterOperationsConditionBuilder,
        statefulset::StatefulSetConditionBuilder,
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};
use tracing::Instrument;

use stackable_nifi_crd::{
    authentication::ResolvedAuthenticationMethod, Container, CurrentlySupportedListenerClasses,
    NifiCluster, NifiConfig, NifiConfigFragment, NifiRole, NifiStatus, APP_NAME, BALANCE_PORT,
    BALANCE_PORT_NAME, HTTPS_PORT, HTTPS_PORT_NAME, LOG_VOLUME_SIZE_IN_MIB, METRICS_PORT,
    METRICS_PORT_NAME, PROTOCOL_PORT, PROTOCOL_PORT_NAME, STACKABLE_LOG_CONFIG_DIR,
    STACKABLE_LOG_DIR,
};

use crate::config::{
    build_bootstrap_conf, build_nifi_properties, build_state_management_xml,
    validated_product_config, NifiRepository, NIFI_BOOTSTRAP_CONF, NIFI_PROPERTIES,
    NIFI_STATE_MANAGEMENT_XML,
};
use crate::product_logging::{extend_role_group_config_map, resolve_vector_aggregator_address};
use crate::{config, OPERATOR_NAME};

pub const CONTROLLER_NAME: &str = "nificluster";

const KEYSTORE_VOLUME_NAME: &str = "keystore";
const KEYSTORE_NIFI_CONTAINER_MOUNT: &str = "/stackable/keystore";
const KEYSTORE_REPORTING_TASK_MOUNT: &str = "/stackable/cert";

const DOCKER_IMAGE_BASE_NAME: &str = "nifi";

pub struct Ctx {
    pub client: Client,
    pub product_config: ProductConfigManager,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("object defines no name"))]
    ObjectHasNoName,
    #[snafu(display("object defines no spec"))]
    ObjectHasNoSpec,
    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,
    #[snafu(display("failed to create cluster resources"))]
    CreateClusterResources {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphanedResources {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to calculate global service name"))]
    GlobalServiceNameNotFound,
    #[snafu(display("failed to apply global Service"))]
    ApplyRoleService {
        source: stackable_operator::error::Error,
    },
    #[snafu(display("failed to update status"))]
    StatusUpdate {
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
    #[snafu(display("failed to prepare NiFi configuration for rolegroup {rolegroup}"))]
    BuildProductConfig {
        source: crate::config::Error,
        rolegroup: RoleGroupRef<NifiCluster>,
    },
    #[snafu(display("illegal container name: [{container_name}]"))]
    IllegalContainerName {
        source: stackable_operator::error::Error,
        container_name: String,
    },
    #[snafu(display("failed to validate resources for {rolegroup}"))]
    ResourceValidation {
        source: fragment::ValidationError,
        rolegroup: RoleGroupRef<NifiCluster>,
    },
    #[snafu(display("failed to resolve and merge config for role and role group"))]
    FailedToResolveConfig { source: stackable_nifi_crd::Error },
    #[snafu(display("failed to resolve the Vector aggregator address"))]
    ResolveVectorAggregatorAddress {
        source: crate::product_logging::Error,
    },
    #[snafu(display("failed to add the logging configuration to the ConfigMap [{cm_name}]"))]
    InvalidLoggingConfig {
        source: crate::product_logging::Error,
        cm_name: String,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VersionChangeState {
    BeginChange,
    Stopped,
    NoChange,
}

pub async fn reconcile_nifi(nifi: Arc<NifiCluster>, ctx: Arc<Ctx>) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let client = &ctx.client;
    let namespace = &nifi
        .metadata
        .namespace
        .clone()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?;

    let resolved_product_image: ResolvedProductImage =
        nifi.spec.image.resolve(DOCKER_IMAGE_BASE_NAME);

    tracing::info!("Checking for sensitive key configuration");
    check_or_generate_sensitive_key(client, &nifi).await?;

    // Handle full restarts for a version change
    let version_change = if let Some(deployed_version) = nifi
        .status
        .as_ref()
        .and_then(|status| status.deployed_version.as_ref())
    {
        if deployed_version != &resolved_product_image.product_version {
            // Check if statefulsets are already scaled to zero, if not - requeue
            let selector = LabelSelector {
                match_expressions: None,
                match_labels: Some(role_selector_labels(
                    nifi.deref(),
                    APP_NAME,
                    &NifiRole::Node.to_string(),
                )),
            };

            // Retrieve the deployed statefulsets to check on the current status of the restart
            let deployed_statefulsets = client
                .list_with_label_selector::<StatefulSet>(namespace, &selector)
                .await
                .context(ApplyRoleServiceSnafu)?;

            // Sum target replicas for all statefulsets
            let target_replicas = deployed_statefulsets
                .iter()
                .filter_map(|statefulset| statefulset.spec.as_ref())
                .filter_map(|spec| spec.replicas)
                .sum::<i32>();

            // Sum current ready replicas for all statefulsets
            let current_replicas = deployed_statefulsets
                .iter()
                .filter_map(|statefulset| statefulset.status.as_ref())
                .map(|status| status.replicas)
                .sum::<i32>();

            // If statefulsets have already been scaled to zero, but have remaining replicas
            // we requeue to wait until a full stop has been performed.
            if target_replicas == 0 && current_replicas > 0 {
                tracing::info!("Cluster is performing a full restart at the moment and still shutting down, remaining replicas: [{}] - requeueing to wait for shutdown to finish", current_replicas);
                return Ok(Action::await_change());
            }

            // Otherwise we either still need to scale the statefulsets to 0 or all replicas have
            // been stopped and we can restart the cluster.
            // Both actions will be taken in the regular reconciliation, so we can simply continue
            // here
            if target_replicas > 0 {
                tracing::info!("Version change detected, we'll need to scale down the cluster for a full restart.");
                VersionChangeState::BeginChange
            } else {
                tracing::info!("Cluster has been stopped for a restart, will scale back up.");
                VersionChangeState::Stopped
            }
        } else {
            // No version change detected, propagate this to the reconciliation
            VersionChangeState::NoChange
        }
    } else {
        // No deployed version set in status, this is probably the first reconciliation ever
        // for this cluster, so just let it progress normally
        tracing::debug!("No deployed version found for this cluster, this is probably the first start, continue reconciliation");
        VersionChangeState::NoChange
    };

    let validated_config = validated_product_config(
        &nifi,
        &resolved_product_image.product_version,
        nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?,
        &ctx.product_config,
    )
    .context(ProductConfigLoadFailedSnafu)?;

    let mut cluster_resources = ClusterResources::new(
        APP_NAME,
        OPERATOR_NAME,
        CONTROLLER_NAME,
        &nifi.object_ref(&()),
        ClusterResourceApplyStrategy::from(&nifi.spec.cluster_operation),
    )
    .context(CreateClusterResourcesSnafu)?;

    let nifi_node_config = validated_config
        .get(&NifiRole::Node.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let node_role_service = build_node_role_service(&nifi, &resolved_product_image)?;
    cluster_resources
        .add(client, node_role_service)
        .await
        .context(ApplyRoleServiceSnafu)?;

    // This is read back to obtain the hosts that we later need to fill in the proxy_hosts variable
    let updated_role_service = client
        .get::<Service>(&nifi.name_any(), namespace)
        .await
        .with_context(|_| MissingServiceSnafu {
            obj_ref: ObjectRef::new(&nifi.name_any()).within(namespace),
        })?;

    let namespace = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;

    let resolved_auth_conf = nifi
        .spec
        .cluster_config
        .authentication
        .method
        .resolve(client, namespace)
        .await
        .context(MaterializeAuthConfigSnafu)?;

    let vector_aggregator_address = resolve_vector_aggregator_address(&nifi, client)
        .await
        .context(ResolveVectorAggregatorAddressSnafu)?;

    let mut ss_cond_builder = StatefulSetConditionBuilder::default();

    for (rolegroup_name, rolegroup_config) in nifi_node_config.iter() {
        let rg_span = tracing::info_span!("rolegroup_span", rolegroup = rolegroup_name.as_str());
        async {
            let rolegroup = nifi.node_rolegroup_ref(rolegroup_name);

            tracing::debug!("Processing rolegroup {}", rolegroup);

            let merged_config = nifi
                .merged_config(&NifiRole::Node, rolegroup_name)
                .context(FailedToResolveConfigSnafu)?;

            let rg_service =
                build_node_rolegroup_service(&nifi, &resolved_product_image, &rolegroup)?;

            let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;

            // This is due to the fact that users might access NiFi via these addresses, if they try to
            // connect from an external machine (not inside the k8s overlay network).
            // Since we cannot predict which of the addresses a user might decide to use we will simply
            // add all of them to the setting for now.
            // For more information see <https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#proxy_configuration>
            let proxy_hosts = get_proxy_hosts(client, &nifi, &updated_role_service).await?;

            let rg_configmap = build_node_rolegroup_config_map(
                &nifi,
                &resolved_product_image,
                &resolved_auth_conf,
                &rolegroup,
                rolegroup_config,
                &merged_config,
                vector_aggregator_address.as_deref(),
                &proxy_hosts,
            )
            .await?;

            let rg_statefulset = build_node_rolegroup_statefulset(
                &nifi,
                &resolved_product_image,
                &rolegroup,
                role,
                rolegroup_config,
                &merged_config,
                &resolved_auth_conf,
                &version_change,
            )
            .await?;

            let reporting_task_job = build_reporting_task_job(
                &nifi,
                &resolved_product_image,
                &rolegroup,
                &resolved_auth_conf,
            )?;

            cluster_resources
                .add(client, rg_service)
                .await
                .with_context(|_| ApplyRoleGroupServiceSnafu {
                    rolegroup: rolegroup.clone(),
                })?;
            cluster_resources
                .add(client, rg_configmap)
                .await
                .with_context(|_| ApplyRoleGroupConfigSnafu {
                    rolegroup: rolegroup.clone(),
                })?;
            ss_cond_builder.add(
                cluster_resources
                    .add(client, rg_statefulset)
                    .await
                    .with_context(|_| ApplyRoleGroupStatefulSetSnafu {
                        rolegroup: rolegroup.clone(),
                    })?,
            );

            client
                .apply_patch(CONTROLLER_NAME, &reporting_task_job, &reporting_task_job)
                .await
                .context(ApplyCreateReportingTaskJobSnafu)?;
            Ok(())
        }
        .instrument(rg_span)
        .await?
    }

    // Remove any orphaned resources that still exist in k8s, but have not been added to
    // the cluster resources during the reconciliation
    // TODO: this doesn't cater for a graceful cluster shrink, for that we'd need to predict
    //  the resources that will be removed and run a disconnect/offload job for those
    //  see https://github.com/stackabletech/nifi-operator/issues/314
    cluster_resources
        .delete_orphaned_resources(client)
        .await
        .context(DeleteOrphanedResourcesSnafu)?;

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&nifi.spec.cluster_operation);

    let conditions = compute_conditions(
        nifi.as_ref(),
        &[&ss_cond_builder, &cluster_operation_cond_builder],
    );

    // Update the deployed product version in the status after everything has been deployed, unless
    // we are still in the process of updating
    let status = if version_change != VersionChangeState::BeginChange {
        NifiStatus {
            deployed_version: Some(resolved_product_image.product_version),
            conditions,
        }
    } else {
        NifiStatus {
            deployed_version: nifi
                .status
                .as_ref()
                .and_then(|status| status.deployed_version.clone()),
            conditions,
        }
    };

    client
        .apply_patch_status(OPERATOR_NAME, &*nifi, &status)
        .await
        .context(StatusUpdateSnafu)?;

    Ok(Action::await_change())
}

/// The node-role service is the primary endpoint that should be used by clients that do not
/// perform internal load balancing including targets outside of the cluster.
pub fn build_node_role_service(
    nifi: &NifiCluster,
    resolved_product_image: &ResolvedProductImage,
) -> Result<Service> {
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
            .with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                &role_name,
                "global",
            ))
            .build(),
        spec: Some(ServiceSpec {
            type_: Some(nifi.spec.cluster_config.listener_class.k8s_service_type()),
            ports: Some(vec![ServicePort {
                name: Some(HTTPS_PORT_NAME.to_string()),
                port: HTTPS_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(role_selector_labels(nifi, APP_NAME, &role_name)),
            external_traffic_policy: match nifi.spec.cluster_config.listener_class {
                CurrentlySupportedListenerClasses::ClusterInternal => None,
                CurrentlySupportedListenerClasses::ExternalUnstable => Some("Local".to_string()),
            },
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// The rolegroup [`ConfigMap`] configures the rolegroup based on the configuration given by the administrator
#[allow(clippy::too_many_arguments)]
async fn build_node_rolegroup_config_map(
    nifi: &NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    resolved_auth_conf: &ResolvedAuthenticationMethod,
    rolegroup: &RoleGroupRef<NifiCluster>,
    rolegroup_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    merged_config: &NifiConfig,
    vector_aggregator_address: Option<&str>,
    proxy_hosts: &str,
) -> Result<ConfigMap> {
    tracing::debug!("building rolegroup configmaps");

    let (login_identity_provider_xml, authorizers_xml) = resolved_auth_conf.get_auth_config();

    let mut cm_builder = ConfigMapBuilder::new();

    cm_builder
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(nifi)
                .name(rolegroup.object_name())
                .ownerreference_from_resource(nifi, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRefSnafu)?
                .with_recommended_labels(build_recommended_labels(
                    nifi,
                    &resolved_product_image.app_version_label,
                    &rolegroup.role,
                    &rolegroup.role_group,
                ))
                .build(),
        )
        .add_data(
            NIFI_BOOTSTRAP_CONF,
            build_bootstrap_conf(
                &merged_config.resources,
                rolegroup_config
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
                &merged_config.resources,
                proxy_hosts,
                rolegroup_config
                    .get(&PropertyNameKind::File(NIFI_PROPERTIES.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_PROPERTIES.to_string(),
                    })?
                    .clone(),
            )
            .with_context(|_| BuildProductConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?,
        )
        .add_data(NIFI_STATE_MANAGEMENT_XML, build_state_management_xml())
        .add_data("login-identity-providers.xml", login_identity_provider_xml)
        .add_data("authorizers.xml", authorizers_xml);

    extend_role_group_config_map(
        rolegroup,
        vector_aggregator_address,
        &merged_config.logging,
        &mut cm_builder,
    )
    .context(InvalidLoggingConfigSnafu {
        cm_name: rolegroup.object_name(),
    })?;

    cm_builder
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
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<NifiCluster>,
) -> Result<Service> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&rolegroup.object_name())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                &rolegroup.role,
                &rolegroup.role_group,
            ))
            .with_label("prometheus.io/scrape", "true")
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
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

const USERDATA_MOUNTPOINT: &str = "/stackable/userdata";

/// The rolegroup [`StatefulSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`Service`] (from [`build_node_rolegroup_service`]).
#[allow(clippy::too_many_arguments)]
async fn build_node_rolegroup_statefulset(
    nifi: &NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
    role: &Role<NifiConfigFragment>,
    rolegroup_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    merged_config: &NifiConfig,
    resolved_auth_conf: &ResolvedAuthenticationMethod,
    version_change_state: &VersionChangeState,
) -> Result<StatefulSet> {
    tracing::debug!("Building statefulset");
    let zookeeper_host = "ZOOKEEPER_HOSTS";
    let zookeeper_chroot = "ZOOKEEPER_CHROOT";

    // get env vars and env overrides
    let mut env_vars: Vec<EnvVar> = rolegroup_config
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
        &nifi.spec.cluster_config.zookeeper_config_map_name,
    ));

    env_vars.push(zookeeper_env_var(
        zookeeper_chroot,
        &nifi.spec.cluster_config.zookeeper_config_map_name,
    ));

    let rolegroup = role.role_groups.get(&rolegroup_ref.role_group);

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

    let sensitive_key_secret = &nifi.spec.cluster_config.sensitive_properties.key_secret;

    let prepare_container_name = Container::Prepare.to_string();
    let mut args = vec![];

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Prepare)
    {
        args.push(product_logging::framework::capture_shell_output(
            STACKABLE_LOG_DIR,
            &prepare_container_name,
            log_config,
        ));
    }

    args.extend(vec![
        "echo Storing password".to_string(),
        format!("echo secret > {keystore_path}/password", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
        "echo Cleaning up truststore - just in case".to_string(),
        format!("rm -f {keystore_path}/truststore.p12", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
        "echo Creating truststore".to_string(),
        format!("keytool -importcert -file {keystore_path}/ca.crt -keystore {keystore_path}/truststore.p12 -storetype pkcs12 -noprompt -alias ca_cert -storepass secret", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
        "echo Creating certificate chain".to_string(),
        format!("cat {keystore_path}/ca.crt {keystore_path}/tls.crt > {keystore_path}/chain.crt", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
        "echo Creating keystore".to_string(),
        format!("openssl pkcs12 -export -in {keystore_path}/chain.crt -inkey {keystore_path}/tls.key -out {keystore_path}/keystore.p12 --passout file:{keystore_path}/password", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
        "echo Cleaning up password".to_string(),
        format!("rm -f {keystore_path}/password", keystore_path=KEYSTORE_NIFI_CONTAINER_MOUNT),
        "echo Replacing config directory".to_string(),
        "cp /conf/* /stackable/nifi/conf".to_string(),
        "ln -sf /stackable/log_config/logback.xml /stackable/nifi/conf/logback.xml".to_string(),
        "echo Replacing nifi.cluster.node.address in nifi.properties".to_string(),
        format!("sed -i \"s/nifi.cluster.node.address=/nifi.cluster.node.address={}/g\" /stackable/nifi/conf/nifi.properties", node_address),
        "echo Replacing nifi.web.https.host in nifi.properties".to_string(),
        format!("sed -i \"s/nifi.web.https.host=0.0.0.0/nifi.web.https.host={}/g\" /stackable/nifi/conf/nifi.properties", node_address),
        "echo Replacing nifi.sensitive.props.key in nifi.properties".to_string(),
        "sed -i \"s|nifi.sensitive.props.key=|nifi.sensitive.props.key=$(cat /stackable/sensitiveproperty/nifiSensitivePropsKey)|g\" /stackable/nifi/conf/nifi.properties".to_string(),
        "echo Replacing 'nifi.zookeeper.connect.string=xxxxxx' in /stackable/nifi/conf/nifi.properties".to_string(),
        format!("sed -i \"s|nifi.zookeeper.connect.string=xxxxxx|nifi.zookeeper.connect.string=${{{}}}|g\" /stackable/nifi/conf/nifi.properties", zookeeper_host),
        "echo Replacing 'nifi.zookeeper.root.node=xxxxxx' in /stackable/nifi/conf/nifi.properties".to_string(),
        format!("sed -i \"s|nifi.zookeeper.root.node=xxxxxx|nifi.zookeeper.root.node=${{{}}}|g\" /stackable/nifi/conf/nifi.properties", zookeeper_chroot),
        "echo Replacing connect string 'xxxxxx' in /stackable/nifi/conf/state-management.xml".to_string(),
        format!("sed -i \"s|xxxxxx|${{{}}}|g\" /stackable/nifi/conf/state-management.xml", zookeeper_host),
        "echo Replacing root node 'yyyyyy' in /stackable/nifi/conf/state-management.xml".to_string(),
        format!("sed -i \"s|yyyyyy|${{{}}}|g\" /stackable/nifi/conf/state-management.xml",zookeeper_chroot)
    ]);

    args.extend_from_slice(
        resolved_auth_conf
            .get_additional_container_args()
            .as_slice(),
    );

    let mut container_prepare =
        ContainerBuilder::new(&prepare_container_name).with_context(|_| {
            IllegalContainerNameSnafu {
                container_name: prepare_container_name.to_string(),
            }
        })?;

    container_prepare
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
        ])
        .add_env_vars(env_vars.clone())
        .args(vec![args.join(" && ")])
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
        .add_volume_mount("conf", "/conf")
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
        .add_volume_mount("activeconf", "/stackable/nifi/conf")
        .add_volume_mount("sensitiveproperty", "/stackable/sensitiveproperty")
        .add_volume_mount("log", STACKABLE_LOG_DIR);

    let nifi_container_name = Container::Nifi.to_string();
    let mut container_builder = ContainerBuilder::new(&nifi_container_name).with_context(|_| {
        IllegalContainerNameSnafu {
            container_name: nifi_container_name,
        }
    })?;

    let container_nifi = container_builder
        .image_from_product_image(resolved_product_image)
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
        .add_volume_mount("log-config", STACKABLE_LOG_CONFIG_DIR)
        .add_volume_mount("log", STACKABLE_LOG_DIR)
        .add_container_port(HTTPS_PORT_NAME, HTTPS_PORT.into())
        .add_container_port(PROTOCOL_PORT_NAME, PROTOCOL_PORT.into())
        .add_container_port(BALANCE_PORT_NAME, BALANCE_PORT.into())
        .add_container_port(METRICS_PORT_NAME, METRICS_PORT.into())
        .liveness_probe(Probe {
            initial_delay_seconds: Some(10),
            period_seconds: Some(10),
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
                ..TCPSocketAction::default()
            }),
            ..Probe::default()
        })
        .startup_probe(Probe {
            initial_delay_seconds: Some(10),
            period_seconds: Some(10),
            failure_threshold: Some(20 * 6),
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
                ..TCPSocketAction::default()
            }),
            ..Probe::default()
        })
        .resources(merged_config.resources.clone().into());

    let mut pod_builder = PodBuilder::new();

    // Add user configured extra volumes if any are specified
    for volume in &nifi.spec.cluster_config.extra_volumes {
        // Extract values into vars so we make it impossible to log something other than
        // what we actually use to create the mounts - maybe paranoid, but hey ..
        let volume_name = &volume.name;
        let mount_point = format!("{USERDATA_MOUNTPOINT}/{}", volume.name);

        tracing::info!(
            ?volume_name,
            ?mount_point,
            ?role,
            "Adding user specified extra volume",
        );
        pod_builder.add_volume(volume.clone());
        container_nifi.add_volume_mount(volume_name, mount_point);
    }

    // We want to add nifi container first for easier defaulting into this container
    pod_builder.add_container(container_nifi.build());

    if let Some(ContainerLogConfig {
        choice:
            Some(ContainerLogConfigChoice::Custom(CustomContainerLogConfig {
                custom: ConfigMapLogConfig { config_map },
            })),
    }) = merged_config.logging.containers.get(&Container::Nifi)
    {
        pod_builder.add_volume(Volume {
            name: "log-config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(config_map.into()),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        });
    } else {
        pod_builder.add_volume(Volume {
            name: "log-config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(rolegroup_ref.object_name()),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        });
    }

    if merged_config.logging.enable_vector_agent {
        pod_builder.add_container(product_logging::framework::vector_container(
            resolved_product_image,
            "config",
            "log",
            merged_config.logging.containers.get(&Container::Vector),
        ));
    }

    resolved_auth_conf.add_volumes_and_mounts(
        &mut pod_builder,
        vec![&mut container_prepare, container_nifi],
    );

    let pod_template = pod_builder
        .metadata_builder(|m| {
            m.with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            ))
        })
        .image_pull_secrets_from_product_image(resolved_product_image)
        .add_init_container(container_prepare.build())
        .affinity(&merged_config.affinity)
        // One volume for the NiFi configuration. A script will later on edit (e.g. nodename)
        // and copy the whole content to the <NIFI_HOME>/conf folder.
        .add_volume(stackable_operator::k8s_openapi::api::core::v1::Volume {
            name: "config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(rolegroup_ref.object_name()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .add_volume(Volume {
            name: "conf".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(rolegroup_ref.object_name()),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        })
        .add_volume(Volume {
            name: "log".to_string(),
            empty_dir: Some(EmptyDirVolumeSource {
                medium: None,
                size_limit: Some(Quantity(format!("{LOG_VOLUME_SIZE_IN_MIB}Mi"))),
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
        .security_context(
            PodSecurityContextBuilder::new()
                .run_as_user(1000)
                .run_as_group(1000)
                .fs_group(1000)
                .build(),
        )
        .build_template();

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/instance".to_string(),
        nifi.metadata
            .name
            .as_deref()
            .with_context(|| ObjectHasNoNameSnafu {})?
            .to_string(),
    );

    Ok(StatefulSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(&rolegroup_ref.object_name())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            ))
            .build(),
        spec: Some(StatefulSetSpec {
            pod_management_policy: Some("Parallel".to_string()),
            replicas: if version_change_state == &VersionChangeState::BeginChange {
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
                merged_config.resources.storage.content_repo.build_pvc(
                    &NifiRepository::Content.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                merged_config.resources.storage.database_repo.build_pvc(
                    &NifiRepository::Database.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                merged_config.resources.storage.flowfile_repo.build_pvc(
                    &NifiRepository::Flowfile.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                merged_config.resources.storage.provenance_repo.build_pvc(
                    &NifiRepository::Provenance.repository(),
                    Some(vec!["ReadWriteOnce"]),
                ),
                merged_config.resources.storage.state_repo.build_pvc(
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
    resolved_product_image: &ResolvedProductImage,
    rolegroup_ref: &RoleGroupRef<NifiCluster>,
    resolved_auth_conf: &ResolvedAuthenticationMethod,
) -> Result<Job> {
    let rolegroup_obj_name = rolegroup_ref.object_name();
    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;
    let product_version = &resolved_product_image.product_version;
    let nifi_connect_url = format!(
        "https://{rolegroup}-0.{rolegroup}.{namespace}.svc.cluster.local:{port}/nifi-api",
        rolegroup = rolegroup_obj_name,
        namespace = namespace,
        port = HTTPS_PORT
    );

    let (admin_username_file, admin_password_file) =
        resolved_auth_conf.get_user_and_password_file_paths();

    let args = vec![
        "/stackable/python/create_nifi_reporting_task.py".to_string(),
        format!("-n {nifi_connect_url}"),
        // In case of the username being simple (e.g. admin) just use it as is
        // If the username is a bind dn (e.g. cn=integrationtest,ou=users,dc=example,dc=org) we have to extract the cn/dn/uid (in this case integrationtest)
        format!(
            "-u $(cat {admin_username_file} | grep -oP '((cn|dn|uid)=\\K[^,]+|.*)' | head -n 1)"
        ),
        format!("-p $(cat {admin_password_file})"),
        format!("-v {product_version}"),
        format!("-m {METRICS_PORT}"),
        format!("-c {KEYSTORE_REPORTING_TASK_MOUNT}/ca.crt"),
    ];
    let mut cb = ContainerBuilder::new("create-reporting-task").with_context(|_| {
        IllegalContainerNameSnafu {
            container_name: "create-reporting-task".to_string(),
        }
    })?;
    cb.image_from_product_image(resolved_product_image)
        .command(vec!["sh".to_string(), "-c".to_string()])
        .args(vec![args.join(" ")])
        // The VolumeMount for the secret operator key store certificates
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_REPORTING_TASK_MOUNT);

    let job_name = format!(
        "{}-create-reporting-task-{}",
        nifi.name_any(),
        product_version.replace('.', "-")
    );

    let mut pd = PodBuilder::new();

    resolved_auth_conf.add_volumes_and_mounts(&mut pd, vec![&mut cb]);

    let mut pod = pd
        .metadata(
            ObjectMetaBuilder::new()
                .name(job_name.clone())
                .namespace_opt(nifi.namespace())
                .build(),
        )
        .image_pull_secrets_from_product_image(resolved_product_image)
        .security_context(PodSecurityContext {
            run_as_user: Some(1000),
            run_as_group: Some(1000),
            fs_group: Some(1000),
            ..PodSecurityContext::default()
        })
        .add_container(cb.build())
        .add_volume(build_keystore_volume(KEYSTORE_VOLUME_NAME))
        .build_template();

    // The PodBuilder doesn't support setting the restart policy yet, so we have to set it like this
    // Feature request: https://github.com/stackabletech/operator-rs/issues/538
    let spec = pod.spec.as_mut().unwrap();
    spec.restart_policy = Some("OnFailure".to_owned());

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
    let sensitive_config = &nifi.spec.cluster_config.sensitive_properties;
    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;

    match client
        .get_opt::<Secret>(&sensitive_config.key_secret, namespace)
        .await
        .with_context(|_| SensitiveKeySecretSnafu {})?
    {
        Some(_) => Ok(false),
        None => {
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
    VolumeBuilder::new(name)
        .csi(CSIVolumeSource {
            driver: "secrets.stackable.tech".to_string(),
            volume_attributes: Some(get_stackable_secret_volume_attributes()),
            ..CSIVolumeSource::default()
        })
        .build()
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
    let mut proxy_setting = vec![nifi
        .node_role_service_fqdn()
        .context(NoRoleServiceFqdnSnafu)?];

    // In case NodePort is used add them as well
    if nifi.spec.cluster_config.listener_class
        == CurrentlySupportedListenerClasses::ExternalUnstable
    {
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
            .list_with_label_selector::<Node>(&(), &selector)
            .await
            .with_context(|_| MissingNodesSnafu {
                obj_ref: ObjectRef::from_obj(nifi),
                selector,
            })?;

        // We need the addresses of all nodes to add these to the NiFi proxy setting
        // Since there is no real convention about how to label these addresses we will simply
        // take all published addresses for now to be on the safe side.
        proxy_setting.extend(
            cluster_nodes
                .into_iter()
                .flat_map(|node| {
                    node.status
                        .unwrap_or_default()
                        .addresses
                        .unwrap_or_default()
                })
                .map(|node_address| format!("{}:{}", node_address.address, external_port)),
        );
    }

    Ok(proxy_setting.join(","))
}

pub fn error_policy(_obj: Arc<NifiCluster>, _error: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(10))
}

fn build_recommended_labels<'a>(
    owner: &'a NifiCluster,
    app_version: &'a str,
    role: &'a str,
    role_group: &'a str,
) -> ObjectLabels<'a, NifiCluster> {
    ObjectLabels {
        owner,
        app_name: APP_NAME,
        app_version,
        operator_name: OPERATOR_NAME,
        controller_name: CONTROLLER_NAME,
        role,
        role_group,
    }
}
