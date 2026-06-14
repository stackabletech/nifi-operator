// TODO: This module can be removed once we don't support NiFi 1.x versions anymore
// It manages the version upgrade procedure for NiFi versions prior to NiFi 2, since rolling upgrade is not supported there yet

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    k8s_openapi::{api::apps::v1::StatefulSet, apimachinery::pkg::apis::meta::v1::LabelSelector},
};

use super::ValidatedCluster;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to fetch deployed StatefulSets"))]
    FetchStatefulsets {
        source: stackable_operator::client::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

// This struct is used for NiFi versions not supporting rolling upgrades since in that case
// we have to manage the restart process ourselves and need to track the state of it
#[derive(Debug, PartialEq, Eq)]
pub enum ClusterVersionUpdateState {
    UpdateRequested,
    UpdateInProgress,
    ClusterStopped,
    NoVersionChange,
}

pub async fn cluster_version_update_state(
    cluster: &ValidatedCluster,
    client: &Client,
    deployed_version: Option<&String>,
) -> Result<ClusterVersionUpdateState> {
    // The version we want to converge to, i.e. the resolved product image version.
    let resolved_version = &cluster.image.product_version;

    // Handle full restarts for a version change
    match deployed_version {
        Some(deployed_version) => {
            if deployed_version != resolved_version {
                // Check if statefulsets are already scaled to zero, if not - requeue
                let selector = LabelSelector {
                    match_expressions: None,
                    match_labels: Some(cluster.role_selector().into()),
                };

                // Retrieve the deployed statefulsets to check on the current status of the restart
                let deployed_statefulsets = client
                    .list_with_label_selector::<StatefulSet>(cluster.namespace.as_ref(), &selector)
                    .await
                    .context(FetchStatefulsetsSnafu)?;

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
                    tracing::info!(
                        "Cluster is performing a full restart at the moment and still shutting down, remaining replicas: [{}] - requeuing to wait for shutdown to finish",
                        current_replicas
                    );
                    return Ok(ClusterVersionUpdateState::UpdateInProgress);
                }

                // Otherwise we either still need to scale the statefulsets to 0 or all replicas have
                // been stopped and we can restart the cluster.
                // Both actions will be taken in the regular reconciliation, so we can simply continue
                // here
                if target_replicas > 0 {
                    tracing::info!(
                        "Version change detected, we'll need to scale down the cluster for a full restart."
                    );
                    Ok(ClusterVersionUpdateState::UpdateRequested)
                } else {
                    tracing::info!("Cluster has been stopped for a restart, will scale back up.");
                    Ok(ClusterVersionUpdateState::ClusterStopped)
                }
            } else {
                // No version change detected, propagate this to the reconciliation
                Ok(ClusterVersionUpdateState::NoVersionChange)
            }
        }
        None => {
            // No deployed version set in status, this is probably the first reconciliation ever
            // for this cluster, so just let it progress normally
            tracing::debug!(
                "No deployed version found for this cluster, this is probably the first start, continue reconciliation"
            );
            Ok(ClusterVersionUpdateState::NoVersionChange)
        }
    }
}
