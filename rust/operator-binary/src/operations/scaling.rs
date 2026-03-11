//! NiFi scaling hooks — drives node offload/disconnect/delete via the NiFi REST API.
//!
//! The scale-down sequence differs between NiFi versions:
//! - NiFi 1.x: CONNECTED → OFFLOADING → OFFLOADED → DISCONNECTING → DISCONNECTED → DELETE
//! - NiFi 2.x: CONNECTED → DISCONNECTING → DISCONNECTED → OFFLOADING → OFFLOADED → DELETE

use snafu::{ResultExt, Snafu};
use stackable_operator::crd::scaler::{HookOutcome, ScalingContext, ScalingHooks};
use tracing::{info, warn};

use super::credentials::{self, NifiCredentials};
use super::nifi_api::{self, NifiApiClient, NifiNode, NifiNodeStatus};
use crate::crd::HTTPS_PORT;

/// Errors from NiFi scaling hook operations.
#[derive(Debug, Snafu)]
pub enum Error {
    /// Failed to read NiFi admin credentials from the Kubernetes Secret.
    #[snafu(display("failed to resolve NiFi admin credentials"))]
    ResolveCredentials { source: credentials::Error },

    /// Failed to connect and authenticate to the NiFi REST API.
    #[snafu(display("failed to connect to NiFi REST API"))]
    NifiApiConnect { source: nifi_api::Error },

    /// Failed to query the NiFi cluster node list.
    #[snafu(display("failed to query NiFi cluster nodes"))]
    GetClusterNodes { source: nifi_api::Error },

    /// Failed to initiate offload for a NiFi node.
    #[snafu(display("failed to offload NiFi node {node_id}"))]
    OffloadNode {
        source: nifi_api::Error,
        node_id: String,
    },

    /// Failed to initiate disconnect for a NiFi node.
    #[snafu(display("failed to disconnect NiFi node {node_id}"))]
    DisconnectNode {
        source: nifi_api::Error,
        node_id: String,
    },

    /// Failed to delete a NiFi node from the cluster.
    #[snafu(display("failed to delete NiFi node {node_id}"))]
    DeleteNode {
        source: nifi_api::Error,
        node_id: String,
    },

    /// A node was in an unexpected status for the current scale-down phase.
    #[snafu(display(
        "NiFi node '{address}' is in unexpected status {status:?} during scale-down phase"
    ))]
    UnexpectedPhaseStatus {
        address: String,
        status: NifiNodeStatus,
    },
}

/// Implements pre/post-scale hooks for NiFi clusters.
///
/// On scale-down, `pre_scale` drives the NiFi REST API to offload, disconnect,
/// and delete the highest-ordinal nodes before the StatefulSet replica count
/// is reduced.
pub struct NifiScalingHooks {
    /// Namespace of the cluster.
    pub namespace: String,
    /// Name of the Kubernetes Secret containing SingleUser credentials.
    pub credentials_secret_name: String,
    /// The StatefulSet name for the role group (e.g., "simple-nifi-node-default").
    pub statefulset_name: String,
    /// The headless service name for the role group
    /// (e.g., "simple-nifi-node-default-headless").
    pub headless_service_name: String,
    /// Kubernetes cluster domain (e.g., "cluster.local").
    pub cluster_domain: String,
    /// NiFi product version (e.g., "1.28.0" or "2.6.0").
    /// Determines the scale-down sequence since NiFi 2.x requires
    /// disconnect-before-offload while 1.x requires offload-before-disconnect.
    pub product_version: String,
}

impl NifiScalingHooks {
    /// Build the FQDN for a NiFi pod by ordinal.
    ///
    /// Format: `{sts_name}-{ordinal}.{headless_svc}.{namespace}.svc.{cluster_domain}`
    ///
    /// # Parameters
    ///
    /// - `ordinal`: The StatefulSet pod ordinal (0-based).
    fn pod_fqdn(&self, ordinal: i32) -> String {
        format!(
            "{sts_name}-{ordinal}.{headless_svc}.{namespace}.svc.{cluster_domain}",
            sts_name = self.statefulset_name,
            headless_svc = self.headless_service_name,
            namespace = self.namespace,
            cluster_domain = self.cluster_domain,
        )
    }

    /// Build the NiFi REST API base URL for a given pod ordinal.
    ///
    /// # Parameters
    ///
    /// - `ordinal`: The StatefulSet pod ordinal whose API endpoint to target.
    fn api_base_url(&self, ordinal: i32) -> String {
        format!("https://{}:{}/nifi-api", self.pod_fqdn(ordinal), HTTPS_PORT)
    }

    /// Whether this is a NiFi 2.x (or later) cluster.
    ///
    /// Parses the major version from `product_version` using semver. Falls back to
    /// `false` (NiFi 1.x behavior) if the version string cannot be parsed.
    fn is_nifi_2(&self) -> bool {
        self.product_version
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 2)
    }

    /// Drive the scale-down: offload, disconnect, and delete nodes with
    /// ordinals >= desired_replicas.
    ///
    /// Uses a phased approach that differs by NiFi version:
    ///
    /// NiFi 1.x: CONNECTED → OFFLOADING → OFFLOADED → DISCONNECTING → DISCONNECTED → DELETE
    /// NiFi 2.x: CONNECTED → DISCONNECTING → DISCONNECTED → OFFLOADING → OFFLOADED → DELETE
    ///
    /// # Parameters
    ///
    /// - `ctx`: Scaling context with client, namespace, and replica counts.
    ///
    /// # Returns
    ///
    /// - [`HookOutcome::Done`] when all target nodes have been removed from the cluster.
    /// - [`HookOutcome::InProgress`] when nodes are still transitioning.
    async fn drive_scale_down(&self, ctx: &ScalingContext<'_>) -> Result<HookOutcome, Error> {
        // 1. Resolve credentials from K8s Secret
        let NifiCredentials { username, password } = credentials::resolve_single_user_credentials(
            ctx.client,
            &self.credentials_secret_name,
            ctx.namespace,
        )
        .await
        .context(ResolveCredentialsSnafu)?;

        // 2. Connect to NiFi REST API via pod-0 (always safe; won't be removed)
        let api = NifiApiClient::connect(self.api_base_url(0), &username, &password)
            .await
            .context(NifiApiConnectSnafu)?;

        // 3. Query cluster nodes
        let nodes = api
            .get_cluster_nodes()
            .await
            .context(GetClusterNodesSnafu)?;

        // 4. Identify target nodes by matching pod FQDNs to NiFi node addresses
        let mut targets = Vec::new();
        for ordinal in ctx.removed_ordinals() {
            let pod_fqdn = self.pod_fqdn(ordinal);
            if let Some(node) = nodes.iter().find(|n| n.address == pod_fqdn) {
                targets.push(node.clone());
            } else {
                let cluster_addresses: Vec<&str> =
                    nodes.iter().map(|n| n.address.as_str()).collect();
                warn!(
                    ordinal,
                    expected_fqdn = %pod_fqdn,
                    ?cluster_addresses,
                    "Target node FQDN not found in NiFi cluster — assuming already removed. \
                     If this is unexpected, check that the headless service name and cluster \
                     domain match the NiFi node addresses."
                );
            }
        }

        if targets.is_empty() {
            return Ok(HookOutcome::Done);
        }

        // 5. Run version-specific phased scale-down
        if self.is_nifi_2() {
            self.drive_scale_down_v2(&api, &targets).await
        } else {
            self.drive_scale_down_v1(&api, &targets).await
        }
    }

    /// NiFi 1.x scale-down sequence.
    ///
    /// # Parameters
    ///
    /// - `api`: Authenticated NiFi API client.
    /// - `targets`: Nodes to decommission (ordinals >= desired replicas).
    async fn drive_scale_down_v1(
        &self,
        api: &NifiApiClient,
        targets: &[NifiNode],
    ) -> Result<HookOutcome, Error> {
        // Phase 1: Offloading — trigger for CONNECTED nodes, wait for all
        let mut any_in_progress = false;
        for node in targets {
            match node.status {
                NifiNodeStatus::Connected => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        "NiFi 1.x: Initiating offload for connected node"
                    );
                    api.set_node_status(&node.node_id, NifiNodeStatus::Offloading)
                        .await
                        .context(OffloadNodeSnafu {
                            node_id: node.node_id.clone(),
                        })?;
                    any_in_progress = true;
                }
                NifiNodeStatus::Connecting | NifiNodeStatus::Offloading => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        status = node.status.as_api_str(),
                        "NiFi 1.x: Node still in progress, waiting"
                    );
                    any_in_progress = true;
                }
                NifiNodeStatus::Offloaded
                | NifiNodeStatus::Disconnecting
                | NifiNodeStatus::Disconnected => {
                    // Already past offload phase
                }
            }
        }

        if any_in_progress {
            return Ok(HookOutcome::InProgress);
        }

        // Phase 2: Disconnecting — trigger for OFFLOADED nodes, wait for all
        let mut any_in_progress = false;
        for node in targets {
            match node.status {
                NifiNodeStatus::Offloaded => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        "NiFi 1.x: Disconnecting offloaded node"
                    );
                    api.set_node_status(&node.node_id, NifiNodeStatus::Disconnecting)
                        .await
                        .context(DisconnectNodeSnafu {
                            node_id: node.node_id.clone(),
                        })?;
                    any_in_progress = true;
                }
                NifiNodeStatus::Disconnecting => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        "NiFi 1.x: Node still disconnecting"
                    );
                    any_in_progress = true;
                }
                NifiNodeStatus::Disconnected => {
                    // Ready for deletion
                }
                other => {
                    return UnexpectedPhaseStatusSnafu {
                        address: node.address.clone(),
                        status: other,
                    }
                    .fail();
                }
            }
        }

        if any_in_progress {
            return Ok(HookOutcome::InProgress);
        }

        // Phase 3: Delete all DISCONNECTED nodes
        self.delete_nodes_with_status(api, targets, NifiNodeStatus::Disconnected)
            .await
    }

    /// NiFi 2.x scale-down sequence.
    ///
    /// # Parameters
    ///
    /// - `api`: Authenticated NiFi API client.
    /// - `targets`: Nodes to decommission (ordinals >= desired replicas).
    async fn drive_scale_down_v2(
        &self,
        api: &NifiApiClient,
        targets: &[NifiNode],
    ) -> Result<HookOutcome, Error> {
        // Phase 1: Disconnecting — trigger for CONNECTED nodes, wait for all
        let mut any_in_progress = false;
        for node in targets {
            match node.status {
                NifiNodeStatus::Connected => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        "NiFi 2.x: Disconnecting connected node"
                    );
                    api.set_node_status(&node.node_id, NifiNodeStatus::Disconnecting)
                        .await
                        .context(DisconnectNodeSnafu {
                            node_id: node.node_id.clone(),
                        })?;
                    any_in_progress = true;
                }
                NifiNodeStatus::Connecting | NifiNodeStatus::Disconnecting => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        status = node.status.as_api_str(),
                        "NiFi 2.x: Node still in progress, waiting"
                    );
                    any_in_progress = true;
                }
                NifiNodeStatus::Disconnected
                | NifiNodeStatus::Offloading
                | NifiNodeStatus::Offloaded => {
                    // Already past disconnect phase
                }
            }
        }

        if any_in_progress {
            return Ok(HookOutcome::InProgress);
        }

        // Phase 2: Offloading — trigger for DISCONNECTED nodes, wait for all
        let mut any_in_progress = false;
        for node in targets {
            match node.status {
                NifiNodeStatus::Disconnected => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        "NiFi 2.x: Offloading disconnected node"
                    );
                    api.set_node_status(&node.node_id, NifiNodeStatus::Offloading)
                        .await
                        .context(OffloadNodeSnafu {
                            node_id: node.node_id.clone(),
                        })?;
                    any_in_progress = true;
                }
                NifiNodeStatus::Offloading => {
                    info!(
                        node_id = %node.node_id,
                        address = %node.address,
                        "NiFi 2.x: Node still offloading"
                    );
                    any_in_progress = true;
                }
                NifiNodeStatus::Offloaded => {
                    // Ready for deletion
                }
                other => {
                    return UnexpectedPhaseStatusSnafu {
                        address: node.address.clone(),
                        status: other,
                    }
                    .fail();
                }
            }
        }

        if any_in_progress {
            return Ok(HookOutcome::InProgress);
        }

        // Phase 3: Delete all OFFLOADED nodes
        self.delete_nodes_with_status(api, targets, NifiNodeStatus::Offloaded)
            .await
    }

    /// Delete all nodes in `targets` that match the given `ready_status`.
    ///
    /// Nodes not matching `ready_status` are silently skipped -- they are assumed
    /// to have already been deleted or to be in an earlier phase.
    ///
    /// # Parameters
    ///
    /// - `api`: Authenticated NiFi API client.
    /// - `targets`: The full set of target nodes for this scale-down operation.
    /// - `ready_status`: Only nodes with this status will be deleted.
    async fn delete_nodes_with_status(
        &self,
        api: &NifiApiClient,
        targets: &[NifiNode],
        ready_status: NifiNodeStatus,
    ) -> Result<HookOutcome, Error> {
        for node in targets {
            if node.status == ready_status {
                info!(
                    node_id = %node.node_id,
                    address = %node.address,
                    "Deleting NiFi node from cluster"
                );
                api.delete_node(&node.node_id)
                    .await
                    .context(DeleteNodeSnafu {
                        node_id: node.node_id.clone(),
                    })?;
            }
        }
        Ok(HookOutcome::Done)
    }
}

impl ScalingHooks for NifiScalingHooks {
    type Error = Error;

    async fn pre_scale(&self, ctx: &ScalingContext<'_>) -> Result<HookOutcome, Error> {
        if !ctx.is_scale_down() {
            return Ok(HookOutcome::Done);
        }
        self.drive_scale_down(ctx).await
    }

    // post_scale: use trait default (returns Done immediately).
    // on_failure: use trait default (no-op).
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hooks(version: &str) -> NifiScalingHooks {
        NifiScalingHooks {
            namespace: "default".to_string(),
            credentials_secret_name: "nifi-users".to_string(),
            statefulset_name: "test-cluster-node-default".to_string(),
            headless_service_name: "test-cluster-node-default-headless".to_string(),
            cluster_domain: "cluster.local".to_string(),
            product_version: version.to_string(),
        }
    }

    #[test]
    fn pod_fqdn_is_correct() {
        let h = hooks("2.6.0");
        assert_eq!(
            h.pod_fqdn(2),
            "test-cluster-node-default-2.test-cluster-node-default-headless.default.svc.cluster.local"
        );
    }

    #[test]
    fn api_base_url_is_correct() {
        let h = hooks("2.6.0");
        assert_eq!(
            h.api_base_url(0),
            "https://test-cluster-node-default-0.test-cluster-node-default-headless.default.svc.cluster.local:8443/nifi-api"
        );
    }

    #[test]
    fn is_nifi_2_detects_version() {
        assert!(hooks("2.6.0").is_nifi_2());
        assert!(hooks("2.0.0").is_nifi_2());
        assert!(hooks("3.0.0").is_nifi_2());
        assert!(!hooks("1.28.0").is_nifi_2());
        assert!(!hooks("1.14.0").is_nifi_2());
        assert!(!hooks("0.9.0").is_nifi_2());
        // Unparseable version falls back to NiFi 1.x behavior
        assert!(!hooks("invalid").is_nifi_2());
    }
}
