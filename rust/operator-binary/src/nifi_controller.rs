//! Ensures that `Pod`s are configured and running for each [`v1alpha1::NifiCluster`].

use std::sync::Arc;

use const_format::concatcp;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    client::Client,
    cluster_resources::ClusterResourceApplyStrategy,
    commons::rbac::build_rbac_resources,
    kube::{
        ResourceExt,
        core::{DeserializeGuard, error_boundary},
        runtime::controller::Action,
    },
    logging::controller::ReconcilerError,
    role_utils::GenericRoleConfig,
    shared::time::Duration,
    status::condition::{
        compute_conditions, operations::ClusterOperationsConditionBuilder,
        statefulset::StatefulSetConditionBuilder,
    },
    v2::{cluster_resources::cluster_resources_new, types::operator::RoleGroupName},
};
use strum::{EnumDiscriminants, IntoStaticStr};
use tracing::Instrument;

use crate::{
    OPERATOR_NAME,
    controller::{
        build,
        build::resource::{
            listener::{build_group_listener, group_listener_name},
            pdb::build_pdb,
            reporting_task::build_maybe_reporting_task,
            service::{build_rolegroup_headless_service, build_rolegroup_metrics_service},
            statefulset::build_node_rolegroup_statefulset,
        },
        controller_name, dereference, operator_name, product_name,
        upgrade::{self, ClusterVersionUpdateState},
        validate,
    },
    crd::{APP_NAME, NifiNodeRoleConfig, NifiRole, NifiStatus, v1alpha1},
    security::{
        authentication::NifiAuthenticationConfig, check_or_generate_oidc_admin_password,
        check_or_generate_sensitive_key,
    },
};

pub const NIFI_CONTROLLER_NAME: &str = "nificluster";
pub const NIFI_FULL_CONTROLLER_NAME: &str = concatcp!(NIFI_CONTROLLER_NAME, '.', OPERATOR_NAME);

pub struct Ctx {
    pub client: Client,
    pub operator_environment: OperatorEnvironmentOptions,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("NifiCluster object is invalid"))]
    InvalidNifiCluster {
        source: error_boundary::InvalidObject,
    },

    #[snafu(display("failed to dereference resources"))]
    Dereference { source: dereference::Error },

    #[snafu(display("failed to validate cluster"))]
    ValidateCluster { source: validate::Error },

    #[snafu(display("failed to delete orphaned resources"))]
    DeleteOrphanedResources {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to update status"))]
    StatusUpdate {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to apply Service for {}", rolegroup))]
    ApplyRoleGroupService {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to build rolegroup ConfigMap for {}", rolegroup))]
    BuildRoleGroupConfigMap {
        source: build::config_map::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to build StatefulSet for {}", rolegroup))]
    BuildStatefulSet {
        source: crate::controller::build::resource::statefulset::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("object has no nodes defined"))]
    NoNodesDefined,

    #[snafu(display("failed to apply ConfigMap for {}", rolegroup))]
    ApplyRoleGroupConfig {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to apply StatefulSet for {}", rolegroup))]
    ApplyRoleGroupStatefulSet {
        source: stackable_operator::cluster_resources::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to apply create ReportingTask service"))]
    ApplyCreateReportingTaskService {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to apply create ReportingTask job"))]
    ApplyCreateReportingTaskJob {
        source: stackable_operator::cluster_resources::Error,
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

    #[snafu(display("failed to apply PodDisruptionBudget"))]
    ApplyPdb {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to get required labels"))]
    GetRequiredLabels {
        source:
            stackable_operator::kvp::KeyValuePairError<stackable_operator::kvp::LabelValueError>,
    },

    #[snafu(display("security failure"))]
    Security { source: crate::security::Error },

    #[snafu(display("reporting task failure"))]
    ReportingTask {
        source: crate::controller::build::resource::reporting_task::Error,
    },

    #[snafu(display("Failed to determine the state of the version upgrade procedure"))]
    ClusterVersionUpdateState { source: upgrade::Error },

    #[snafu(display("failed to apply group listener"))]
    ApplyGroupListener {
        source: stackable_operator::cluster_resources::Error,
    },

    #[snafu(display("failed to configure listener"))]
    ListenerConfiguration {
        source: crate::controller::build::resource::listener::Error,
    },
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

    // dereference (client required)
    let dereferenced_objects = dereference::dereference(client, nifi)
        .await
        .context(DereferenceSnafu)?;

    // validate (no Kubernetes API calls required)
    let validated_cluster =
        validate::validate(nifi, &dereferenced_objects, &ctx.operator_environment)
            .context(ValidateClusterSnafu)?;

    let resolved_product_image = &validated_cluster.image;
    let authentication_config = &validated_cluster.cluster_config.authentication;

    tracing::info!("Checking for sensitive key configuration");
    check_or_generate_sensitive_key(client, nifi, &validated_cluster.namespace)
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
            &validated_cluster.namespace,
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

    let mut cluster_resources = cluster_resources_new(
        &product_name(),
        &operator_name(),
        &controller_name(),
        &validated_cluster.name,
        &validated_cluster.namespace,
        &validated_cluster.uid,
        ClusterResourceApplyStrategy::from(&nifi.spec.cluster_operation),
        &nifi.spec.object_overrides,
    );

    if let NifiAuthenticationConfig::Oidc { .. } = authentication_config {
        check_or_generate_oidc_admin_password(client, nifi, &validated_cluster.namespace)
            .await
            .context(SecuritySnafu)?;
    }

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
    let node_role_group_configs = validated_cluster
        .role_group_configs
        .get(&nifi_role)
        .context(NoNodesDefinedSnafu)?;
    for (role_group_name, rg) in node_role_group_configs.iter() {
        let rg_span = tracing::info_span!("rolegroup_span", rolegroup = role_group_name.as_ref());
        async {
            tracing::debug!("Processing rolegroup {role_group_name}");

            let rg_headless_service =
                build_rolegroup_headless_service(&validated_cluster, role_group_name);

            let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;

            let rg_configmap = build::config_map::build_rolegroup_config_map(
                &validated_cluster,
                rg,
                &client.kubernetes_cluster_info,
            )
            .context(BuildRoleGroupConfigMapSnafu {
                rolegroup: role_group_name.clone(),
            })?;

            let role_group = role.role_groups.get(role_group_name.as_ref());
            let replicas =
                if cluster_version_update_state == ClusterVersionUpdateState::UpdateRequested {
                    Some(0)
                } else {
                    role_group.and_then(|rg| rg.replicas).map(i32::from)
                };

            let rg_statefulset = build_node_rolegroup_statefulset(
                &validated_cluster,
                &client.kubernetes_cluster_info,
                role,
                rg,
                rolling_upgrade_supported,
                replicas,
                &rbac_sa.name_any(),
            )
            .await
            .with_context(|_| BuildStatefulSetSnafu {
                rolegroup: role_group_name.clone(),
            })?;

            let rg_metrics_service =
                build_rolegroup_metrics_service(&validated_cluster, role_group_name);

            cluster_resources
                .add(client, rg_metrics_service)
                .await
                .with_context(|_| ApplyRoleGroupServiceSnafu {
                    rolegroup: role_group_name.clone(),
                })?;

            cluster_resources
                .add(client, rg_headless_service)
                .await
                .with_context(|_| ApplyRoleGroupServiceSnafu {
                    rolegroup: role_group_name.clone(),
                })?;

            cluster_resources
                .add(client, rg_configmap)
                .await
                .with_context(|_| ApplyRoleGroupConfigSnafu {
                    rolegroup: role_group_name.clone(),
                })?;

            // Note: The StatefulSet needs to be applied after all ConfigMaps and Secrets it mounts
            // to prevent unnecessary Pod restarts.
            // See https://github.com/stackabletech/commons-operator/issues/111 for details.
            ss_cond_builder.add(
                cluster_resources
                    .add(client, rg_statefulset)
                    .await
                    .with_context(|_| ApplyRoleGroupStatefulSetSnafu {
                        rolegroup: role_group_name.clone(),
                    })?,
            );

            Ok(())
        }
        .instrument(rg_span)
        .await?
    }

    let role_config = nifi.role_config(&nifi_role);
    if let Some(NifiNodeRoleConfig {
        common: GenericRoleConfig {
            pod_disruption_budget: pdb,
        },
        listener_class,
    }) = role_config
    {
        if let Some(pdb) = build_pdb(pdb, &validated_cluster, &nifi_role) {
            cluster_resources
                .add(client, pdb)
                .await
                .context(ApplyPdbSnafu)?;
        }

        let role_group_listener = build_group_listener(
            &validated_cluster,
            listener_class.to_owned(),
            group_listener_name(&validated_cluster, &nifi_role.to_string()),
        )
        .context(ListenerConfigurationSnafu)?;

        cluster_resources
            .add(client, role_group_listener)
            .await
            .context(ApplyGroupListenerSnafu)?;
    }

    // Only add the reporting task in case it is enabled.
    if nifi.spec.cluster_config.create_reporting_task_job.enabled {
        if let Some((reporting_task_job, reporting_task_service)) = build_maybe_reporting_task(
            &validated_cluster,
            &client.kubernetes_cluster_info,
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
            deployed_version: Some(resolved_product_image.product_version.clone()),
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
