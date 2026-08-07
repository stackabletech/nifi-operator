//! Ensures that `Pod`s are configured and running for each [`v1alpha1::NifiCluster`].

use std::sync::Arc;

use const_format::concatcp;
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    client::Client,
    cluster_resources::ClusterResourceApplyStrategy,
    kube::{
        core::{DeserializeGuard, error_boundary},
        runtime::controller::Action,
    },
    logging::controller::ReconcilerError,
    shared::time::Duration,
    status::condition::{
        compute_conditions, operations::ClusterOperationsConditionBuilder,
        statefulset::StatefulSetConditionBuilder,
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    OPERATOR_NAME,
    controller::{
        apply::{self, Applier, ensure_secrets},
        build, dereference, validate,
    },
    crd::{NifiStatus, v1alpha1},
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

    #[snafu(display("failed to update status"))]
    StatusUpdate {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to build the Kubernetes resources"))]
    BuildResources { source: build::Error },

    #[snafu(display("failed to apply the Kubernetes resources"))]
    ApplyResources { source: apply::Error },
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

    // build (no Kubernetes API calls required)
    let resources = build::build(&validated_cluster).context(BuildResourcesSnafu)?;

    // apply (client required)
    ensure_secrets(client, &validated_cluster)
        .await
        .context(ApplyResourcesSnafu)?;

    let applied = Applier::new(
        client,
        &validated_cluster,
        ClusterResourceApplyStrategy::from(&nifi.spec.cluster_operation),
        &nifi.spec.object_overrides,
    )
    .apply(resources)
    .await
    .context(ApplyResourcesSnafu)?;

    let mut ss_cond_builder = StatefulSetConditionBuilder::default();
    for stateful_set in &applied.stateful_sets {
        ss_cond_builder.add(stateful_set.clone());
    }

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&nifi.spec.cluster_operation);

    let conditions = compute_conditions(nifi, &[&ss_cond_builder, &cluster_operation_cond_builder]);

    let status = NifiStatus {
        deployed_version: Some(validated_cluster.deployed_product_version.clone()),
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
