//! Ensures that `Pod`s are configured and running for each [`v1alpha1::NifiCluster`].

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use const_format::concatcp;
use indoc::formatdoc;
use product_config::{
    ProductConfigManager,
    types::PropertyNameKind,
    writer::{PropertiesWriterError, to_java_properties_string},
};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{
        self,
        configmap::ConfigMapBuilder,
        meta::ObjectMetaBuilder,
        pod::{
            PodBuilder, container::ContainerBuilder, resources::ResourceRequirementsBuilder,
            security::PodSecurityContextBuilder, volume::SecretFormat,
        },
    },
    client::Client,
    cluster_resources::{ClusterResourceApplyStrategy, ClusterResources},
    commons::{product_image_selection::ResolvedProductImage, rbac::build_rbac_resources},
    config::fragment,
    crd::{authentication::oidc, git_sync},
    k8s_openapi::{
        DeepMerge,
        api::{
            apps::v1::{StatefulSet, StatefulSetSpec, StatefulSetUpdateStrategy},
            core::v1::{
                ConfigMap, ConfigMapKeySelector, ConfigMapVolumeSource, EmptyDirVolumeSource,
                EnvVar, EnvVarSource, Node, ObjectFieldSelector, Probe, SecretVolumeSource,
                Service, ServicePort, ServiceSpec, TCPSocketAction, Volume,
            },
        },
        apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
    },
    kube::{
        Resource, ResourceExt,
        api::ListParams,
        core::{DeserializeGuard, error_boundary},
        runtime::{controller::Action, reflector::ObjectRef},
    },
    kvp::{Label, Labels, ObjectLabels},
    logging::controller::ReconcilerError,
    memory::{BinaryMultiple, MemoryQuantity},
    product_config_utils::env_vars_from_rolegroup_config,
    product_logging::{
        self,
        framework::{
            LoggingError, create_vector_shutdown_file_command, remove_vector_shutdown_file_command,
        },
        spec::{
            ConfigMapLogConfig, ContainerLogConfig, ContainerLogConfigChoice,
            CustomContainerLogConfig,
        },
    },
    role_utils::{GenericRoleConfig, JavaCommonConfig, Role, RoleGroupRef},
    status::condition::{
        compute_conditions, operations::ClusterOperationsConditionBuilder,
        statefulset::StatefulSetConditionBuilder,
    },
    time::Duration,
    utils::{COMMON_BASH_TRAP_FUNCTIONS, cluster_info::KubernetesClusterInfo},
};
use strum::{EnumDiscriminants, IntoStaticStr};
use tracing::Instrument;

use crate::{
    OPERATOR_NAME,
    config::{
        self, JVM_SECURITY_PROPERTIES_FILE, NIFI_BOOTSTRAP_CONF, NIFI_CONFIG_DIRECTORY,
        NIFI_PROPERTIES, NIFI_PYTHON_WORKING_DIRECTORY, NIFI_STATE_MANAGEMENT_XML, NifiRepository,
        build_bootstrap_conf, build_nifi_properties, build_state_management_xml,
        validated_product_config,
    },
    crd::{
        APP_NAME, BALANCE_PORT, BALANCE_PORT_NAME, Container, CurrentlySupportedListenerClasses,
        HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME, NifiConfig,
        NifiConfigFragment, NifiRole, NifiStatus, PROTOCOL_PORT, PROTOCOL_PORT_NAME,
        STACKABLE_LOG_CONFIG_DIR, STACKABLE_LOG_DIR, authentication::AuthenticationClassResolved,
        v1alpha1,
    },
    operations::{
        graceful_shutdown::add_graceful_shutdown_config,
        pdb::add_pdbs,
        upgrade::{self, ClusterVersionUpdateState},
    },
    product_logging::extend_role_group_config_map,
    reporting_task::{self, build_maybe_reporting_task, build_reporting_task_service_name},
    security::{
        authentication::{
            AUTHORIZERS_XML_FILE_NAME, LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME,
            NifiAuthenticationConfig, STACKABLE_SERVER_TLS_DIR, STACKABLE_TLS_STORE_PASSWORD,
        },
        authorization::NifiAuthorizationConfig,
        build_tls_volume, check_or_generate_oidc_admin_password, check_or_generate_sensitive_key,
        tls::{KEYSTORE_NIFI_CONTAINER_MOUNT, KEYSTORE_VOLUME_NAME, TRUSTSTORE_VOLUME_NAME},
    },
};

pub const NIFI_CONTROLLER_NAME: &str = "nificluster";
pub const NIFI_FULL_CONTROLLER_NAME: &str = concatcp!(NIFI_CONTROLLER_NAME, '.', OPERATOR_NAME);

const DOCKER_IMAGE_BASE_NAME: &str = "nifi";
const LOG_VOLUME_NAME: &str = "log";

pub struct Ctx {
    pub client: Client,
    pub product_config: ProductConfigManager,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("missing secret lifetime"))]
    MissingSecretLifetime,

    #[snafu(display("NifiCluster object is invalid"))]
    InvalidNifiCluster {
        source: error_boundary::InvalidObject,
    },

    #[snafu(display("object defines no name"))]
    ObjectHasNoName,

    #[snafu(display("object defines no spec"))]
    ObjectHasNoSpec,

    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("failed to create cluster resources"))]
    CreateClusterResources {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphanedResources {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply global Service"))]
    ApplyRoleService {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to fetch deployed StatefulSets"))]
    FetchStatefulsets {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to update status"))]
    StatusUpdate {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to apply Service for {}", rolegroup))]
    ApplyRoleGroupService {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to build ConfigMap for {}", rolegroup))]
    BuildRoleGroupConfig {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("object has no nodes defined"))]
    NoNodesDefined,

    #[snafu(display("failed to apply ConfigMap for {}", rolegroup))]
    ApplyRoleGroupConfig {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to apply StatefulSet for {}", rolegroup))]
    ApplyRoleGroupStatefulSet {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to apply create ReportingTask service"))]
    ApplyCreateReportingTaskService {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply create ReportingTask job"))]
    ApplyCreateReportingTaskJob {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("Failed to load product config"))]
    ProductConfigLoadFailed {
        #[snafu(source(from(config::Error, Box::new)))]
        source: Box<config::Error>,
    },

    #[snafu(display("Failed to find information about file [{}] in product config", kind))]
    ProductConfigKindNotSpecified { kind: String },

    #[snafu(display("Failed to find any nodes in cluster {obj_ref}",))]
    MissingNodes {
        source: stackable_operator::client::Error,
        obj_ref: ObjectRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("Failed to find service {obj_ref}"))]
    MissingService {
        source: stackable_operator::client::Error,
        obj_ref: ObjectRef<Service>,
    },

    #[snafu(display("Failed to find an external port to use for proxy hosts"))]
    ExternalPort,

    #[snafu(display("Could not build role service fqdn"))]
    NoRoleServiceFqdn,

    #[snafu(display("Bootstrap configuration error"))]
    BootstrapConfig {
        #[snafu(source(from(config::Error, Box::new)))]
        source: Box<config::Error>,
    },

    #[snafu(display("failed to prepare NiFi configuration for rolegroup {rolegroup}"))]
    BuildProductConfig {
        #[snafu(source(from(config::Error, Box::new)))]
        source: Box<config::Error>,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("illegal container name: [{container_name}]"))]
    IllegalContainerName {
        source: stackable_operator::builder::pod::container::Error,
        container_name: String,
    },

    #[snafu(display("failed to validate resources for {rolegroup}"))]
    ResourceValidation {
        source: fragment::ValidationError,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to resolve and merge config for role and role group"))]
    FailedToResolveConfig { source: crate::crd::Error },

    #[snafu(display("invalid git-sync specification"))]
    InvalidGitSyncSpec { source: git_sync::v1alpha1::Error },

    #[snafu(display("vector agent is enabled but vector aggregator ConfigMap is missing"))]
    VectorAggregatorConfigMapMissing,

    #[snafu(display("failed to add the logging configuration to the ConfigMap [{cm_name}]"))]
    InvalidLoggingConfig {
        source: crate::product_logging::Error,
        cm_name: String,
    },

    #[snafu(display("failed to patch service account"))]
    ApplyServiceAccount {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to patch role binding"))]
    ApplyRoleBinding {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to build RBAC resources"))]
    BuildRbacResources {
        source: stackable_operator::commons::rbac::Error,
    },

    #[snafu(display(
        "failed to serialize [{JVM_SECURITY_PROPERTIES_FILE}] for {}",
        rolegroup
    ))]
    JvmSecurityProperties {
        source: PropertiesWriterError,
        rolegroup: String,
    },

    #[snafu(display("Invalid NiFi Authentication Configuration"))]
    InvalidNifiAuthenticationConfig {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("Invalid NiFi Authorization Configuration"))]
    InvalidNifiAuthorizationConfig {
        source: crate::security::authorization::Error,
    },

    #[snafu(display("Failed to resolve NiFi Authentication Configuration"))]
    FailedResolveNifiAuthenticationConfig {
        source: crate::crd::authentication::Error,
    },

    #[snafu(display("failed to create PodDisruptionBudget"))]
    FailedToCreatePdb {
        source: crate::operations::pdb::Error,
    },

    #[snafu(display("failed to configure graceful shutdown"))]
    GracefulShutdown {
        source: crate::operations::graceful_shutdown::Error,
    },

    #[snafu(display("failed to build metadata"))]
    MetadataBuild {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to get required labels"))]
    GetRequiredLabels {
        source:
            stackable_operator::kvp::KeyValuePairError<stackable_operator::kvp::LabelValueError>,
    },

    #[snafu(display("failed to build labels"))]
    LabelBuild {
        source: stackable_operator::kvp::LabelError,
    },

    #[snafu(display("failed to add Authentication Volumes and VolumeMounts"))]
    AddAuthVolumes {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("security failure"))]
    Security { source: crate::security::Error },

    #[snafu(display("reporting task failure"))]
    ReportingTask {
        source: crate::reporting_task::Error,
    },

    #[snafu(display("failed to configure logging"))]
    ConfigureLogging { source: LoggingError },

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },

    #[snafu(display("Failed to determine the state of the version upgrade procedure"))]
    ClusterVersionUpdateState { source: upgrade::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_nifi(
    nifi: Arc<DeserializeGuard<v1alpha1::NifiCluster>>,
    ctx: Arc<Ctx>,
) -> Result<Action> {
    tracing::info!("Starting reconcile");
    let nifi = nifi
        .0
        .as_ref()
        .map_err(error_boundary::InvalidObject::clone)
        .context(InvalidNifiClusterSnafu)?;

    let client = &ctx.client;
    let namespace = &nifi
        .metadata
        .namespace
        .clone()
        .with_context(|| ObjectHasNoNamespaceSnafu {})?;

    let resolved_product_image: ResolvedProductImage = nifi
        .spec
        .image
        .resolve(DOCKER_IMAGE_BASE_NAME, crate::built_info::PKG_VERSION);

    tracing::info!("Checking for sensitive key configuration");
    check_or_generate_sensitive_key(client, nifi)
        .await
        .context(SecuritySnafu)?;

    // If rolling upgrade is supported, kubernetes takes care of the cluster scaling automatically
    // otherwise the operator handles it
    // manage our own flow for upgrade from 1.x.x to 1.x.x/2.x.x
    // TODO: this can be removed once 1.x.x is longer supported
    let mut cluster_version_update_state = ClusterVersionUpdateState::NoVersionChange;
    let deployed_version = nifi
        .status
        .as_ref()
        .and_then(|status| status.deployed_version.as_ref());
    let rolling_upgrade_supported = resolved_product_image.product_version.starts_with("2.")
        && deployed_version.is_some_and(|v| v.starts_with("2."));

    if !rolling_upgrade_supported {
        cluster_version_update_state = upgrade::cluster_version_update_state(
            nifi,
            client,
            &resolved_product_image.product_version,
            deployed_version,
        )
        .await
        .context(ClusterVersionUpdateStateSnafu)?;

        if cluster_version_update_state == ClusterVersionUpdateState::UpdateInProgress {
            return Ok(Action::await_change());
        }
    }
    // end todo

    let validated_config = validated_product_config(
        nifi,
        &resolved_product_image.product_version,
        nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?,
        &ctx.product_config,
    )
    .context(ProductConfigLoadFailedSnafu)?;

    let mut cluster_resources = ClusterResources::new(
        APP_NAME,
        OPERATOR_NAME,
        NIFI_CONTROLLER_NAME,
        &nifi.object_ref(&()),
        ClusterResourceApplyStrategy::from(&nifi.spec.cluster_operation),
    )
    .context(CreateClusterResourcesSnafu)?;

    let nifi_node_config = validated_config
        .get(&NifiRole::Node.to_string())
        .map(Cow::Borrowed)
        .unwrap_or_default();

    let node_role_service = build_node_role_service(nifi, &resolved_product_image)?;
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

    let authentication_config = NifiAuthenticationConfig::try_from(
        AuthenticationClassResolved::from(nifi, client)
            .await
            .context(FailedResolveNifiAuthenticationConfigSnafu)?,
    )
    .context(InvalidNifiAuthenticationConfigSnafu)?;

    if let NifiAuthenticationConfig::Oidc { .. } = authentication_config {
        check_or_generate_oidc_admin_password(client, nifi)
            .await
            .context(SecuritySnafu)?;
    }

    let authorization_config =
        NifiAuthorizationConfig::from(&nifi.spec.cluster_config.authorization);

    let (rbac_sa, rbac_rolebinding) = build_rbac_resources(
        nifi,
        APP_NAME,
        cluster_resources
            .get_required_labels()
            .context(GetRequiredLabelsSnafu)?,
    )
    .context(BuildRbacResourcesSnafu)?;

    let rbac_sa = cluster_resources
        .add(client, rbac_sa)
        .await
        .context(ApplyServiceAccountSnafu)?;

    cluster_resources
        .add(client, rbac_rolebinding)
        .await
        .context(ApplyRoleBindingSnafu)?;

    let mut ss_cond_builder = StatefulSetConditionBuilder::default();

    let nifi_role = NifiRole::Node;
    for (rolegroup_name, rolegroup_config) in nifi_node_config.iter() {
        let rg_span = tracing::info_span!("rolegroup_span", rolegroup = rolegroup_name.as_str());
        async {
            let rolegroup = nifi.node_rolegroup_ref(rolegroup_name);

            tracing::debug!("Processing rolegroup {}", rolegroup);

            let merged_config = nifi
                .merged_config(&NifiRole::Node, rolegroup_name)
                .context(FailedToResolveConfigSnafu)?;

            let git_sync_resources = git_sync::v1alpha1::GitSyncResources::new(
                &nifi.spec.cluster_config.custom_components_git_sync,
                &resolved_product_image,
                &env_vars_from_rolegroup_config(rolegroup_config),
                &[],
                LOG_VOLUME_NAME,
                &merged_config.logging.for_container(&Container::GitSync),
            )
            .context(InvalidGitSyncSpecSnafu)?;

            let rg_service =
                build_node_rolegroup_service(nifi, &resolved_product_image, &rolegroup)?;

            let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;

            // This is due to the fact that users might access NiFi via these addresses, if they try to
            // connect from an external machine (not inside the k8s overlay network).
            // Since we cannot predict which of the addresses a user might decide to use we will simply
            // add all of them to the setting for now.
            // For more information see <https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#proxy_configuration>
            let proxy_hosts = get_proxy_hosts(client, nifi, &updated_role_service).await?;

            let rg_configmap = build_node_rolegroup_config_map(
                nifi,
                &resolved_product_image,
                &authentication_config,
                &authorization_config,
                role,
                &rolegroup,
                rolegroup_config,
                &merged_config,
                &proxy_hosts,
                &git_sync_resources,
            )
            .await?;

            let role_group = role.role_groups.get(&rolegroup.role_group);
            let replicas =
                if cluster_version_update_state == ClusterVersionUpdateState::UpdateRequested {
                    Some(0)
                } else {
                    role_group.and_then(|rg| rg.replicas).map(i32::from)
                };

            let rg_statefulset = build_node_rolegroup_statefulset(
                nifi,
                &resolved_product_image,
                &client.kubernetes_cluster_info,
                &rolegroup,
                role,
                rolegroup_config,
                &merged_config,
                &authentication_config,
                &authorization_config,
                rolling_upgrade_supported,
                replicas,
                &rbac_sa.name_any(),
                &git_sync_resources,
            )
            .await?;

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

            Ok(())
        }
        .instrument(rg_span)
        .await?
    }

    let role_config = nifi.role_config(&nifi_role);
    if let Some(GenericRoleConfig {
        pod_disruption_budget: pdb,
    }) = role_config
    {
        add_pdbs(pdb, nifi, &nifi_role, client, &mut cluster_resources)
            .await
            .context(FailedToCreatePdbSnafu)?;
    }

    // Only add the reporting task in case it is enabled.
    if nifi.spec.cluster_config.create_reporting_task_job.enabled {
        if let Some((reporting_task_job, reporting_task_service)) = build_maybe_reporting_task(
            nifi,
            &resolved_product_image,
            &client.kubernetes_cluster_info,
            &authentication_config,
            &rbac_sa.name_any(),
        )
        .context(ReportingTaskSnafu)?
        {
            cluster_resources
                .add(client, reporting_task_service)
                .await
                .context(ApplyCreateReportingTaskServiceSnafu)?;

            cluster_resources
                .add(client, reporting_task_job)
                .await
                .context(ApplyCreateReportingTaskJobSnafu)?;
        }
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

    let conditions = compute_conditions(nifi, &[&ss_cond_builder, &cluster_operation_cond_builder]);

    // Update the deployed product version in the status after everything has been deployed, unless
    // we are still in the process of updating
    let status = if cluster_version_update_state != ClusterVersionUpdateState::UpdateRequested {
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
        .apply_patch_status(OPERATOR_NAME, nifi, &status)
        .await
        .context(StatusUpdateSnafu)?;

    Ok(Action::await_change())
}

/// The node-role service is the primary endpoint that should be used by clients that do not
/// perform internal load balancing including targets outside of the cluster.
pub fn build_node_role_service(
    nifi: &v1alpha1::NifiCluster,
    resolved_product_image: &ResolvedProductImage,
) -> Result<Service> {
    let role_name = NifiRole::Node.to_string();

    let role_svc_name = nifi.node_role_service_name();
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
            .context(MetadataBuildSnafu)?
            .build(),
        spec: Some(ServiceSpec {
            type_: Some(nifi.spec.cluster_config.listener_class.k8s_service_type()),
            ports: Some(vec![ServicePort {
                name: Some(HTTPS_PORT_NAME.to_string()),
                port: HTTPS_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(
                Labels::role_selector(nifi, APP_NAME, &role_name)
                    .context(LabelBuildSnafu)?
                    .into(),
            ),
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
    nifi: &v1alpha1::NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    authentication_config: &NifiAuthenticationConfig,
    authorization_config: &NifiAuthorizationConfig,
    role: &Role<NifiConfigFragment, GenericRoleConfig, JavaCommonConfig>,
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
    rolegroup_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    merged_config: &NifiConfig,
    proxy_hosts: &str,
    git_sync_resources: &git_sync::v1alpha1::GitSyncResources,
) -> Result<ConfigMap> {
    tracing::debug!("building rolegroup configmaps");

    let login_identity_provider_xml = authentication_config
        .get_authentication_config()
        .context(InvalidNifiAuthenticationConfigSnafu)?;

    let authorizers_xml = authorization_config
        .get_authorizers_config(authentication_config)
        .context(InvalidNifiAuthorizationConfigSnafu)?;

    let jvm_sec_props: BTreeMap<String, Option<String>> = rolegroup_config
        .get(&PropertyNameKind::File(
            JVM_SECURITY_PROPERTIES_FILE.to_string(),
        ))
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, Some(v)))
        .collect();

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
                .context(MetadataBuildSnafu)?
                .build(),
        )
        .add_data(
            NIFI_BOOTSTRAP_CONF,
            build_bootstrap_conf(
                merged_config,
                rolegroup_config
                    .get(&PropertyNameKind::File(NIFI_BOOTSTRAP_CONF.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_BOOTSTRAP_CONF.to_string(),
                    })?
                    .clone(),
                role,
                &rolegroup.role_group,
            )
            .context(BootstrapConfigSnafu)?,
        )
        .add_data(
            NIFI_PROPERTIES,
            build_nifi_properties(
                &nifi.spec,
                &merged_config.resources,
                proxy_hosts,
                authentication_config,
                rolegroup_config
                    .get(&PropertyNameKind::File(NIFI_PROPERTIES.to_string()))
                    .with_context(|| ProductConfigKindNotSpecifiedSnafu {
                        kind: NIFI_PROPERTIES.to_string(),
                    })?
                    .clone(),
                resolved_product_image.product_version.as_ref(),
                git_sync_resources,
            )
            .with_context(|_| BuildProductConfigSnafu {
                rolegroup: rolegroup.clone(),
            })?,
        )
        .add_data(
            NIFI_STATE_MANAGEMENT_XML,
            build_state_management_xml(&nifi.spec.cluster_config.clustering_backend),
        )
        .add_data(
            LOGIN_IDENTITY_PROVIDERS_XML_FILE_NAME,
            login_identity_provider_xml,
        )
        .add_data(AUTHORIZERS_XML_FILE_NAME, authorizers_xml)
        .add_data(
            JVM_SECURITY_PROPERTIES_FILE,
            to_java_properties_string(jvm_sec_props.iter()).with_context(|_| {
                JvmSecurityPropertiesSnafu {
                    rolegroup: rolegroup.role_group.clone(),
                }
            })?,
        );

    extend_role_group_config_map(rolegroup, &merged_config.logging, &mut cm_builder).context(
        InvalidLoggingConfigSnafu {
            cm_name: rolegroup.object_name(),
        },
    )?;

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
    nifi: &v1alpha1::NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
) -> Result<Service> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
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
            .context(MetadataBuildSnafu)?
            .with_label(Label::try_from(("prometheus.io/scrape", "true")).context(LabelBuildSnafu)?)
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
            selector: Some(
                Labels::role_group_selector(nifi, APP_NAME, &rolegroup.role, &rolegroup.role_group)
                    .context(LabelBuildSnafu)?
                    .into(),
            ),
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
    nifi: &v1alpha1::NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    cluster_info: &KubernetesClusterInfo,
    rolegroup_ref: &RoleGroupRef<v1alpha1::NifiCluster>,
    role: &Role<NifiConfigFragment, GenericRoleConfig, JavaCommonConfig>,
    rolegroup_config: &HashMap<PropertyNameKind, BTreeMap<String, String>>,
    merged_config: &NifiConfig,
    authentication_config: &NifiAuthenticationConfig,
    authorization_config: &NifiAuthorizationConfig,
    rolling_update_supported: bool,
    replicas: Option<i32>,
    service_account_name: &str,
    git_sync_resources: &git_sync::v1alpha1::GitSyncResources,
) -> Result<StatefulSet> {
    tracing::debug!("Building statefulset");
    let role_group = role.role_groups.get(&rolegroup_ref.role_group);

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

    // Needed for the `containerdebug` process to log it's tracing information to.
    env_vars.push(EnvVar {
        name: "CONTAINERDEBUG_LOG_DIRECTORY".to_string(),
        value: Some(format!("{STACKABLE_LOG_DIR}/containerdebug")),
        ..Default::default()
    });

    env_vars.push(EnvVar {
        name: "STACKLET_NAME".to_string(),
        value: Some(nifi.name_unchecked().to_string()),
        ..Default::default()
    });

    match &nifi.spec.cluster_config.clustering_backend {
        v1alpha1::NifiClusteringBackend::ZooKeeper {
            zookeeper_config_map_name,
        } => {
            let zookeeper_env_var = |name: &str| EnvVar {
                name: name.to_string(),
                value_from: Some(EnvVarSource {
                    config_map_key_ref: Some(ConfigMapKeySelector {
                        name: zookeeper_config_map_name.to_string(),
                        key: name.to_string(),
                        ..ConfigMapKeySelector::default()
                    }),
                    ..EnvVarSource::default()
                }),
                ..EnvVar::default()
            };
            env_vars.push(zookeeper_env_var("ZOOKEEPER_HOSTS"));
            env_vars.push(zookeeper_env_var("ZOOKEEPER_CHROOT"));
        }
        v1alpha1::NifiClusteringBackend::Kubernetes {} => {}
    }

    if let NifiAuthenticationConfig::Oidc { oidc, .. } = authentication_config {
        env_vars.extend(
            oidc::v1alpha1::AuthenticationProvider::client_credentials_env_var_mounts(
                oidc.client_credentials_secret_ref.clone(),
            ),
        );
    }

    env_vars.extend(authorization_config.get_env_vars());

    let node_address = format!(
        "$POD_NAME.{}-node-{}.{}.svc.{}",
        rolegroup_ref.cluster.name,
        rolegroup_ref.role_group,
        &nifi
            .metadata
            .namespace
            .as_ref()
            .context(ObjectHasNoNamespaceSnafu)?,
        cluster_info.cluster_domain,
    );

    let sensitive_key_secret = &nifi.spec.cluster_config.sensitive_properties.key_secret;

    let prepare_container_name = Container::Prepare.to_string();
    let mut prepare_args = vec![];

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Prepare)
    {
        prepare_args.push(product_logging::framework::capture_shell_output(
            STACKABLE_LOG_DIR,
            &prepare_container_name,
            log_config,
        ));
    }

    prepare_args.extend(vec![
        // The source directory is a secret-op mount and we do not want to write / add anything in there
        // Therefore we import all the contents to a truststore in "writeable" empty dirs.
        // Keytool is only barking if a password is not set for the destination truststore (which we set)
        // and do provide an empty password for the source truststore coming from the secret-operator.
        // Using no password will result in a warning.
        format!("echo Importing {KEYSTORE_NIFI_CONTAINER_MOUNT}/keystore.p12 to {STACKABLE_SERVER_TLS_DIR}/keystore.p12"),
        format!("cp {KEYSTORE_NIFI_CONTAINER_MOUNT}/keystore.p12 {STACKABLE_SERVER_TLS_DIR}/keystore.p12"),
        format!("echo Importing {KEYSTORE_NIFI_CONTAINER_MOUNT}/truststore.p12 to {STACKABLE_SERVER_TLS_DIR}/truststore.p12"),
        // secret-operator currently encrypts keystores with RC2, which NiFi is unable to read: https://github.com/stackabletech/nifi-operator/pull/510
        // As a workaround, reencrypt the keystore with keytool.
        // keytool crashes if the target truststore already exists (covering up the true error
        // if the init container fails later on in the script), so delete it first.
        format!("test ! -e {STACKABLE_SERVER_TLS_DIR}/truststore.p12 || rm {STACKABLE_SERVER_TLS_DIR}/truststore.p12"),
        format!("keytool -importkeystore -srckeystore {KEYSTORE_NIFI_CONTAINER_MOUNT}/truststore.p12 -destkeystore {STACKABLE_SERVER_TLS_DIR}/truststore.p12 -srcstorepass {STACKABLE_TLS_STORE_PASSWORD} -deststorepass {STACKABLE_TLS_STORE_PASSWORD}"),

        "echo Replacing config directory".to_string(),
        "cp /conf/* /stackable/nifi/conf".to_string(),
        "test -L /stackable/nifi/conf/logback.xml || ln -sf /stackable/log_config/logback.xml /stackable/nifi/conf/logback.xml".to_string(),
        format!(r#"export NODE_ADDRESS="{node_address}""#),
    ]);

    // This commands needs to go first, as they might set env variables needed by the templating
    prepare_args.extend_from_slice(
        authentication_config
            .get_additional_container_args()
            .as_slice(),
    );

    prepare_args.extend(vec![
        "echo Templating config files".to_string(),
        "config-utils template /stackable/nifi/conf/nifi.properties".to_string(),
        "config-utils template /stackable/nifi/conf/state-management.xml".to_string(),
        "config-utils template /stackable/nifi/conf/login-identity-providers.xml".to_string(),
        "config-utils template /stackable/nifi/conf/authorizers.xml".to_string(),
        "config-utils template /stackable/nifi/conf/security.properties".to_string(),
    ]);

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
        .args(vec![prepare_args.join(" && ")])
        .add_volume_mount(
            NifiRepository::Flowfile.repository(),
            NifiRepository::Flowfile.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Database.repository(),
            NifiRepository::Database.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Content.repository(),
            NifiRepository::Content.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Provenance.repository(),
            NifiRepository::Provenance.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::State.repository(),
            NifiRepository::State.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount("conf", "/conf")
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount("activeconf", NIFI_CONFIG_DIRECTORY)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount("sensitiveproperty", "/stackable/sensitiveproperty")
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(TRUSTSTORE_VOLUME_NAME, STACKABLE_SERVER_TLS_DIR)
        .context(AddVolumeMountSnafu)?
        .resources(
            ResourceRequirementsBuilder::new()
                .with_cpu_request("500m")
                .with_cpu_limit("2000m")
                .with_memory_request("4096Mi")
                .with_memory_limit("4096Mi")
                .build(),
        );

    let nifi_container_name = Container::Nifi.to_string();
    let mut container_builder = ContainerBuilder::new(&nifi_container_name).with_context(|_| {
        IllegalContainerNameSnafu {
            container_name: nifi_container_name,
        }
    })?;

    let nifi_args = vec![formatdoc! {"
            {COMMON_BASH_TRAP_FUNCTIONS}
            {remove_vector_shutdown_file_command}
            prepare_signal_handlers
            containerdebug --output={STACKABLE_LOG_DIR}/containerdebug-state.json --loop &
            bin/nifi.sh run &
            wait_for_termination $!
            {create_vector_shutdown_file_command}
            ",
    remove_vector_shutdown_file_command =
        remove_vector_shutdown_file_command(STACKABLE_LOG_DIR),
    create_vector_shutdown_file_command =
        create_vector_shutdown_file_command(STACKABLE_LOG_DIR),
    }];
    let container_nifi = container_builder
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-x".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
        ])
        .args(nifi_args)
        .add_env_vars(env_vars)
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Flowfile.repository(),
            NifiRepository::Flowfile.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Database.repository(),
            NifiRepository::Database.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Content.repository(),
            NifiRepository::Content.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::Provenance.repository(),
            NifiRepository::Provenance.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            NifiRepository::State.repository(),
            NifiRepository::State.mount_path(),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount("activeconf", NIFI_CONFIG_DIRECTORY)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount("log-config", STACKABLE_LOG_CONFIG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(TRUSTSTORE_VOLUME_NAME, STACKABLE_SERVER_TLS_DIR)
        .context(AddVolumeMountSnafu)?
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
    add_graceful_shutdown_config(merged_config, &mut pod_builder).context(GracefulShutdownSnafu)?;

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
        pod_builder
            .add_volume(volume.clone())
            .context(AddVolumeSnafu)?;
        container_nifi
            .add_volume_mount(volume_name, mount_point)
            .context(AddVolumeMountSnafu)?;
    }

    let volume_name = "nifi-python-working-directory".to_string();
    pod_builder
        .add_empty_dir_volume(&volume_name, None)
        .context(AddVolumeSnafu)?;
    container_nifi
        .add_volume_mount(&volume_name, NIFI_PYTHON_WORKING_DIRECTORY)
        .context(AddVolumeMountSnafu)?;

    container_nifi
        .add_volume_mounts(git_sync_resources.git_content_volume_mounts.to_owned())
        .context(AddVolumeMountSnafu)?;

    // We want to add nifi container first for easier defaulting into this container
    pod_builder.add_container(container_nifi.build());

    for container in git_sync_resources.git_sync_containers.iter().cloned() {
        pod_builder.add_container(container);
    }
    for container in git_sync_resources.git_sync_init_containers.iter().cloned() {
        pod_builder.add_init_container(container);
    }
    pod_builder
        .add_volumes(git_sync_resources.git_content_volumes.to_owned())
        .context(AddVolumeSnafu)?;

    if let Some(ContainerLogConfig {
        choice:
            Some(ContainerLogConfigChoice::Custom(CustomContainerLogConfig {
                custom: ConfigMapLogConfig { config_map },
            })),
    }) = merged_config.logging.containers.get(&Container::Nifi)
    {
        pod_builder
            .add_volume(Volume {
                name: "log-config".to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: config_map.clone(),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            })
            .context(AddVolumeSnafu)?;
    } else {
        pod_builder
            .add_volume(Volume {
                name: "log-config".to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: rolegroup_ref.object_name(),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            })
            .context(AddVolumeSnafu)?;
    }

    if merged_config.logging.enable_vector_agent {
        match &nifi.spec.cluster_config.vector_aggregator_config_map_name {
            Some(vector_aggregator_config_map_name) => {
                pod_builder.add_container(
                    product_logging::framework::vector_container(
                        resolved_product_image,
                        "config",
                        LOG_VOLUME_NAME,
                        merged_config.logging.containers.get(&Container::Vector),
                        ResourceRequirementsBuilder::new()
                            .with_cpu_request("250m")
                            .with_cpu_limit("500m")
                            .with_memory_request("128Mi")
                            .with_memory_limit("128Mi")
                            .build(),
                        vector_aggregator_config_map_name,
                    )
                    .context(ConfigureLoggingSnafu)?,
                );
            }
            None => {
                VectorAggregatorConfigMapMissingSnafu.fail()?;
            }
        }
    }

    authentication_config
        .add_volumes_and_mounts(&mut pod_builder, vec![
            &mut container_prepare,
            container_nifi,
        ])
        .context(AddAuthVolumesSnafu)?;

    let metadata = ObjectMetaBuilder::new()
        .with_recommended_labels(build_recommended_labels(
            nifi,
            &resolved_product_image.app_version_label,
            &rolegroup_ref.role,
            &rolegroup_ref.role_group,
        ))
        .context(MetadataBuildSnafu)?
        .build();

    let requested_secret_lifetime = merged_config
        .requested_secret_lifetime
        .context(MissingSecretLifetimeSnafu)?;
    let nifi_cluster_name = nifi.name_any();
    pod_builder
        .metadata(metadata)
        .image_pull_secrets_from_product_image(resolved_product_image)
        .add_init_container(container_prepare.build())
        .affinity(&merged_config.affinity)
        // One volume for the NiFi configuration. A script will later on edit (e.g. nodename)
        // and copy the whole content to the <NIFI_HOME>/conf folder.
        .add_volume(stackable_operator::k8s_openapi::api::core::v1::Volume {
            name: "config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: rolegroup_ref.object_name(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .context(AddVolumeSnafu)?
        .add_volume(Volume {
            name: "conf".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: rolegroup_ref.object_name(),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        })
        .context(AddVolumeSnafu)?
        .add_empty_dir_volume(
            LOG_VOLUME_NAME,
            // Set volume size to higher than theoretically necessary to avoid running out of disk space as log rotation triggers are only checked by Logback every 5s.
            Some(
                MemoryQuantity {
                    value: 500.0,
                    unit: BinaryMultiple::Mebi,
                }
                .into(),
            ),
        )
        .context(AddVolumeSnafu)?
        // One volume for the keystore and truststore data configmap
        .add_volume(
            build_tls_volume(
                nifi,
                KEYSTORE_VOLUME_NAME,
                vec![
                    &nifi_cluster_name,
                    &build_reporting_task_service_name(&nifi_cluster_name),
                ],
                SecretFormat::TlsPkcs12,
                &requested_secret_lifetime,
            )
            .context(SecuritySnafu)?,
        )
        .context(AddVolumeSnafu)?
        .add_empty_dir_volume(TRUSTSTORE_VOLUME_NAME, None)
        .context(AddVolumeSnafu)?
        .add_volume(Volume {
            name: "sensitiveproperty".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(sensitive_key_secret.to_string()),
                ..SecretVolumeSource::default()
            }),
            ..Volume::default()
        })
        .context(AddVolumeSnafu)?
        .add_volume(Volume {
            empty_dir: Some(EmptyDirVolumeSource {
                medium: None,
                size_limit: None,
            }),
            name: "activeconf".to_string(),
            ..Volume::default()
        })
        .context(AddVolumeSnafu)?
        .service_account_name(service_account_name)
        .security_context(PodSecurityContextBuilder::new().fs_group(1000).build());

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/instance".to_string(),
        nifi.metadata
            .name
            .as_deref()
            .with_context(|| ObjectHasNoNameSnafu {})?
            .to_string(),
    );

    let mut pod_template = pod_builder.build_template();
    pod_template.merge_from(role.config.pod_overrides.clone());
    if let Some(role_group) = role_group {
        pod_template.merge_from(role_group.config.pod_overrides.clone());
    }

    Ok(StatefulSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(rolegroup_ref.object_name())
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                &rolegroup_ref.role,
                &rolegroup_ref.role_group,
            ))
            .context(MetadataBuildSnafu)?
            .build(),
        spec: Some(StatefulSetSpec {
            pod_management_policy: Some("Parallel".to_string()),
            replicas,
            selector: LabelSelector {
                match_labels: Some(
                    Labels::role_group_selector(
                        nifi,
                        APP_NAME,
                        &rolegroup_ref.role,
                        &rolegroup_ref.role_group,
                    )
                    .context(LabelBuildSnafu)?
                    .into(),
                ),
                ..LabelSelector::default()
            },
            service_name: Some(rolegroup_ref.object_name()),
            template: pod_template,
            update_strategy: Some(StatefulSetUpdateStrategy {
                type_: if rolling_update_supported {
                    Some("RollingUpdate".to_string())
                } else {
                    Some("OnDelete".to_string())
                },
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
    nifi: &v1alpha1::NifiCluster,
    nifi_service: &Service,
) -> Result<String> {
    let host_header_check = nifi.spec.cluster_config.host_header_check.clone();

    if host_header_check.allow_all {
        tracing::info!(
            "spec.clusterConfig.hostHeaderCheck.allowAll is set to true. All proxy hosts will be allowed."
        );
        if !host_header_check.additional_allowed_hosts.is_empty() {
            tracing::info!(
                "spec.clusterConfig.hostHeaderCheck.additionalAllowedHosts is ignored and only '*' is added to the allow-list."
            )
        }
        return Ok("*".to_string());
    }

    let node_role_service_fqdn = nifi
        .node_role_service_fqdn(&client.kubernetes_cluster_info)
        .context(NoRoleServiceFqdnSnafu)?;
    let reporting_task_service_name = reporting_task::build_reporting_task_fqdn_service_name(
        nifi,
        &client.kubernetes_cluster_info,
    )
    .context(ReportingTaskSnafu)?;
    let mut proxy_hosts_set = HashSet::from([
        node_role_service_fqdn.clone(),
        format!("{node_role_service_fqdn}:{HTTPS_PORT}"),
        format!("{reporting_task_service_name}:{HTTPS_PORT}"),
    ]);

    proxy_hosts_set.extend(host_header_check.additional_allowed_hosts);

    // In case NodePort is used add them as well
    if nifi.spec.cluster_config.listener_class
        == CurrentlySupportedListenerClasses::ExternalUnstable
    {
        let external_port = external_node_port(nifi_service)?;

        let cluster_nodes = client
            .list::<Node>(&(), &ListParams::default())
            .await
            .with_context(|_| MissingNodesSnafu {
                obj_ref: ObjectRef::from_obj(nifi),
            })?;

        // We need the addresses of all nodes to add these to the NiFi proxy setting
        // Since there is no real convention about how to label these addresses we will simply
        // take all published addresses for now to be on the safe side.
        proxy_hosts_set.extend(
            cluster_nodes
                .into_iter()
                .flat_map(|node| {
                    node.status
                        .unwrap_or_default()
                        .addresses
                        .unwrap_or_default()
                })
                .map(|node_address| format!("{}:{external_port}", node_address.address)),
        );
    }

    let mut proxy_hosts = Vec::from_iter(proxy_hosts_set);
    proxy_hosts.sort();

    Ok(proxy_hosts.join(","))
}

pub fn error_policy(
    _obj: Arc<DeserializeGuard<v1alpha1::NifiCluster>>,
    error: &Error,
    _ctx: Arc<Ctx>,
) -> Action {
    match error {
        // root object is invalid, will be requeued when modified anyway
        Error::InvalidNifiCluster { .. } => Action::await_change(),

        _ => Action::requeue(*Duration::from_secs(10)),
    }
}

pub fn build_recommended_labels<'a>(
    owner: &'a v1alpha1::NifiCluster,
    app_version: &'a str,
    role: &'a str,
    role_group: &'a str,
) -> ObjectLabels<'a, v1alpha1::NifiCluster> {
    ObjectLabels {
        owner,
        app_name: APP_NAME,
        app_version,
        operator_name: OPERATOR_NAME,
        controller_name: NIFI_CONTROLLER_NAME,
        role,
        role_group,
    }
}
