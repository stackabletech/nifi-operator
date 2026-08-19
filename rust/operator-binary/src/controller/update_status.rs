//! The update_status step in the NifiCluster controller.

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    status::condition::{
        compute_conditions, operations::ClusterOperationsConditionBuilder,
        statefulset::StatefulSetConditionBuilder,
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    NIFI_OPERATOR_NAME,
    controller::{Applied, KubernetesResources, ValidatedCluster},
    crd::{NifiStatus, v1alpha1},
};

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
pub enum Error {
    #[snafu(display("failed to update status"))]
    ApplyStatus {
        source: stackable_operator::client::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Computes the cluster status from the applied resources and patches it onto the
/// [`v1alpha1::NifiCluster`]. Takes [`KubernetesResources<Applied>`] so the type system proves the
/// status derives from applied resources, not merely built ones.
///
/// Unlike the sibling operators this also reports the deployed product version, which is why it
/// takes the [`ValidatedCluster`] as well.
pub async fn update_status(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
    cluster: &ValidatedCluster,
    applied: &KubernetesResources<Applied>,
) -> Result<()> {
    let mut ss_cond_builder = StatefulSetConditionBuilder::default();
    for stateful_set in &applied.stateful_sets {
        ss_cond_builder.add(stateful_set.clone());
    }

    let cluster_operation_cond_builder =
        ClusterOperationsConditionBuilder::new(&nifi.spec.cluster_operation);

    let status = NifiStatus {
        deployed_version: Some(cluster.deployed_product_version.clone()),
        conditions: compute_conditions(nifi, &[&ss_cond_builder, &cluster_operation_cond_builder]),
    };

    client
        .apply_patch_status(NIFI_OPERATOR_NAME, nifi, &status)
        .await
        .context(ApplyStatusSnafu)?;

    Ok(())
}
