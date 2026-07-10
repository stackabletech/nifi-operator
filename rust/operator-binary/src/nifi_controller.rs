//! Ensures that `Pod`s are configured and running for each [`v1alpha1::NifiCluster`].

use std::{str::FromStr, sync::Arc};

use const_format::concatcp;
use snafu::{ResultExt, Snafu};
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
    shared::time::Duration,
    status::condition::{
        compute_conditions, operations::ClusterOperationsConditionBuilder,
        statefulset::StatefulSetConditionBuilder,
    },
    v2::{cluster_resources::cluster_resources_new, types::operator::ProductVersion},
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    OPERATOR_NAME,
    controller::{build, controller_name, dereference, operator_name, product_name, validate},
    crd::{APP_NAME, NifiStatus, v1alpha1},
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

    #[snafu(display("failed to build the Kubernetes resources"))]
    BuildResources { source: build::Error },

    #[snafu(display("failed to apply Kubernetes resource"))]
    ApplyResource {
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

    #[snafu(display("failed to get required labels"))]
    GetRequiredLabels {
        source:
            stackable_operator::kvp::KeyValuePairError<stackable_operator::kvp::LabelValueError>,
    },

    #[snafu(display("security failure"))]
    Security { source: crate::security::Error },
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
    check_or_generate_sensitive_key(
        client,
        &validated_cluster.cluster_config.sensitive_properties,
        &validated_cluster.namespace,
    )
    .await
    .context(SecuritySnafu)?;

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
        check_or_generate_oidc_admin_password(
            client,
            &validated_cluster.name,
            &validated_cluster.namespace,
        )
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

    let resources =
        build::build(&validated_cluster, &rbac_sa.name_any()).context(BuildResourcesSnafu)?;

    let mut ss_cond_builder = StatefulSetConditionBuilder::default();

    // Apply order: everything before StatefulSets, StatefulSets last. A StatefulSet must be applied
    // after all ConfigMaps and Secrets it mounts, otherwise the Pods restart unnecessarily.
    // See https://github.com/stackabletech/commons-operator/issues/111 for details.
    for service in resources.services {
        cluster_resources
            .add(client, service)
            .await
            .context(ApplyResourceSnafu)?;
    }
    for listener in resources.listeners {
        cluster_resources
            .add(client, listener)
            .await
            .context(ApplyResourceSnafu)?;
    }
    for config_map in resources.config_maps {
        cluster_resources
            .add(client, config_map)
            .await
            .context(ApplyResourceSnafu)?;
    }
    for pdb in resources.pod_disruption_budgets {
        cluster_resources
            .add(client, pdb)
            .await
            .context(ApplyResourceSnafu)?;
    }
    for stateful_set in resources.stateful_sets {
        ss_cond_builder.add(
            cluster_resources
                .add(client, stateful_set)
                .await
                .context(ApplyResourceSnafu)?,
        );
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

    let status = NifiStatus {
        deployed_version: Some(
            ProductVersion::from_str(&resolved_product_image.product_version)
                .expect("the resolved product version is a valid product version label value"),
        ),
        conditions,
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
