//! NiFi REST API client for cluster management operations.
//!
//! Provides a thin wrapper around the NiFi REST API endpoints
//! needed for scaling operations (node offload, disconnect, delete).

use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use tracing::{debug, warn};

/// Errors from NiFi REST API operations.
#[derive(Debug, Snafu)]
pub enum Error {
    /// Failed to construct the reqwest HTTP client.
    #[snafu(display("failed to build HTTP client"))]
    BuildHttpClient { source: reqwest::Error },

    /// The authentication HTTP request to NiFi failed at the transport level.
    #[snafu(display("failed to authenticate with NiFi at {url}"))]
    Authenticate { source: reqwest::Error, url: String },

    /// NiFi returned a non-success HTTP status during authentication.
    #[snafu(display("NiFi authentication returned non-success status {status} at {url}: {body}"))]
    AuthenticateStatus {
        status: u16,
        url: String,
        body: String,
    },

    /// The cluster nodes query HTTP request failed at the transport level.
    #[snafu(display("failed to query NiFi cluster nodes at {url}"))]
    GetClusterNodes { source: reqwest::Error, url: String },

    /// NiFi returned a non-success HTTP status for the cluster nodes query.
    #[snafu(display("NiFi cluster node query returned non-success status {status}: {body}"))]
    GetClusterNodesStatus { status: u16, body: String },

    /// A NiFi node reported a status string not recognized by [`NifiNodeStatus`].
    #[snafu(display("NiFi node with address '{address}' has unexpected status '{raw_status}'"))]
    UnexpectedNodeStatus { address: String, raw_status: String },

    /// The node status update HTTP request failed at the transport level.
    #[snafu(display("failed to update NiFi node {node_id} status to {target_status}"))]
    UpdateNodeStatus {
        source: reqwest::Error,
        node_id: String,
        target_status: String,
    },

    /// NiFi returned a non-success HTTP status for the node status update.
    #[snafu(display(
        "NiFi node {node_id} status update to {target_status} returned non-success status {status}: {body}"
    ))]
    UpdateNodeStatusHttp {
        status: u16,
        node_id: String,
        target_status: String,
        body: String,
    },

    /// The node deletion HTTP request failed at the transport level.
    #[snafu(display("failed to delete NiFi node {node_id}"))]
    DeleteNode {
        source: reqwest::Error,
        node_id: String,
    },

    /// NiFi returned a non-success HTTP status for the node deletion.
    #[snafu(display("NiFi node {node_id} deletion returned non-success status {status}: {body}"))]
    DeleteNodeStatus {
        status: u16,
        node_id: String,
        body: String,
    },
}

/// NiFi node status values used in the REST API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NifiNodeStatus {
    /// Node is connected and participating in the cluster.
    Connected,
    /// Node is in the process of joining the cluster.
    Connecting,
    /// Node is in the process of leaving the cluster.
    Disconnecting,
    /// Node has left the cluster but still exists in the cluster registry.
    Disconnected,
    /// Node is migrating its data (flowfiles) to other nodes.
    Offloading,
    /// Node has completed data migration and holds no flowfiles.
    Offloaded,
}

impl NifiNodeStatus {
    /// Parse a NiFi REST API status string (e.g. `"CONNECTED"`) into the typed enum.
    ///
    /// Returns `None` for unrecognized status strings.
    pub fn from_api_str(s: &str) -> Option<Self> {
        match s {
            "CONNECTED" => Some(Self::Connected),
            "CONNECTING" => Some(Self::Connecting),
            "DISCONNECTING" => Some(Self::Disconnecting),
            "DISCONNECTED" => Some(Self::Disconnected),
            "OFFLOADING" => Some(Self::Offloading),
            "OFFLOADED" => Some(Self::Offloaded),
            _ => None,
        }
    }

    /// Return the uppercase NiFi REST API representation (e.g. `"CONNECTED"`).
    pub fn as_api_str(&self) -> &'static str {
        match self {
            Self::Connected => "CONNECTED",
            Self::Connecting => "CONNECTING",
            Self::Disconnecting => "DISCONNECTING",
            Self::Disconnected => "DISCONNECTED",
            Self::Offloading => "OFFLOADING",
            Self::Offloaded => "OFFLOADED",
        }
    }
}

/// A NiFi cluster node as returned by the cluster API.
#[derive(Debug, Clone)]
pub struct NifiNode {
    /// Opaque node identifier assigned by NiFi (UUID format).
    pub node_id: String,
    /// The node's address as known to the NiFi cluster, typically a pod FQDN.
    pub address: String,
    /// The node's current cluster membership status.
    pub status: NifiNodeStatus,
}

// ── NiFi REST API JSON structures ───────────────────────────────────────

/// Top-level JSON response from `/controller/cluster`.
#[derive(Debug, Deserialize)]
struct ClusterResponse {
    cluster: ClusterBody,
}

/// The `cluster` object within a [`ClusterResponse`].
#[derive(Debug, Deserialize)]
struct ClusterBody {
    nodes: Vec<NodeResponse>,
}

/// A single node entry in the NiFi cluster API response.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeResponse {
    node_id: String,
    address: String,
    status: String,
}

/// Request body for updating a node's cluster status.
#[derive(Debug, Serialize)]
struct UpdateNodeStatusRequest {
    node: UpdateNodeBody,
}

/// The `node` object within an [`UpdateNodeStatusRequest`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateNodeBody {
    node_id: String,
    status: String,
}

/// Extract the HTTP status code and response body text from an error response.
///
/// Consumes the response. If the body cannot be read, a fallback string is returned.
async fn read_error_body(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
    (status, body)
}

/// Client for NiFi REST API cluster management operations.
pub struct NifiApiClient {
    /// Shared HTTP client with connection pooling. TLS verification is disabled.
    http: reqwest::Client,
    /// NiFi REST API base URL (e.g. `https://pod-0.svc:8443/nifi-api`).
    base_url: String,
    /// Bearer token obtained during [`connect`](Self::connect).
    bearer_token: String,
}

impl NifiApiClient {
    /// Build an HTTP client (skipping TLS verification) and authenticate
    /// using SingleUser credentials.
    ///
    /// `base_url` should be e.g. `https://<pod-0-fqdn>:8443/nifi-api`.
    ///
    /// # Parameters
    ///
    /// - `base_url`: NiFi REST API base URL, e.g. `"https://pod-0.svc:8443/nifi-api"`.
    /// - `username`: SingleUser authentication username (plaintext).
    /// - `password`: SingleUser authentication password (plaintext).
    ///
    /// # Security
    ///
    /// TLS certificate verification is intentionally disabled because NiFi pods use
    /// self-signed certificates generated by the Stackable secret operator.
    pub async fn connect(base_url: String, username: &str, password: &str) -> Result<Self, Error> {
        // TODO(#1): This bypasses all TLS verification, including the mTLS enforced by the
        // rest of the NiFi operator via keystores/truststores. Should use the CA cert from
        // the Stackable secret operator to verify the server certificate instead.
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .context(BuildHttpClientSnafu)?;

        let token_url = format!("{base_url}/access/token");
        debug!(url = %token_url, "Authenticating with NiFi");
        let resp = http
            .post(&token_url)
            .form(&[("username", username), ("password", password)])
            .send()
            .await
            .context(AuthenticateSnafu {
                url: token_url.clone(),
            })?;

        if !resp.status().is_success() {
            let (status, body) = read_error_body(resp).await;
            warn!(status, body = %body, url = %token_url, "NiFi authentication failed");
            return AuthenticateStatusSnafu {
                status,
                url: token_url,
                body,
            }
            .fail();
        }

        let bearer_token = resp
            .text()
            .await
            .context(AuthenticateSnafu { url: token_url })?;
        debug!("NiFi authentication successful");

        Ok(Self {
            http,
            base_url,
            bearer_token,
        })
    }

    /// Retrieve all nodes in the NiFi cluster.
    ///
    /// Returns all nodes known to the NiFi cluster. Nodes with unrecognized status
    /// strings cause an [`Error::UnexpectedNodeStatus`] error.
    pub async fn get_cluster_nodes(&self) -> Result<Vec<NifiNode>, Error> {
        let url = format!("{}/controller/cluster", self.base_url);
        debug!(url = %url, "Querying NiFi cluster nodes");
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.bearer_token)
            .send()
            .await
            .context(GetClusterNodesSnafu { url: url.clone() })?;

        if !resp.status().is_success() {
            let (status, body) = read_error_body(resp).await;
            warn!(status, body = %body, "NiFi cluster node query failed");
            return GetClusterNodesStatusSnafu { status, body }.fail();
        }

        let body: ClusterResponse = resp.json().await.context(GetClusterNodesSnafu { url })?;

        let nodes = body
            .cluster
            .nodes
            .into_iter()
            .map(|n| {
                let status = NifiNodeStatus::from_api_str(&n.status).ok_or_else(|| {
                    Error::UnexpectedNodeStatus {
                        address: n.address.clone(),
                        raw_status: n.status,
                    }
                })?;
                Ok(NifiNode {
                    node_id: n.node_id,
                    address: n.address,
                    status,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        debug!(
            node_count = nodes.len(),
            nodes = ?nodes.iter().map(|n| format!("{}({})={}", n.address, n.node_id, n.status.as_api_str())).collect::<Vec<_>>(),
            "NiFi cluster nodes retrieved"
        );

        Ok(nodes)
    }

    /// Update the status of a node (e.g. OFFLOADING, DISCONNECTING).
    ///
    /// # Parameters
    ///
    /// - `node_id`: The NiFi node identifier (UUID) from [`NifiNode::node_id`].
    /// - `status`: The target status to request (e.g. [`NifiNodeStatus::Offloading`]).
    ///   NiFi may reject invalid transitions with a non-success HTTP status.
    pub async fn set_node_status(
        &self,
        node_id: &str,
        status: NifiNodeStatus,
    ) -> Result<(), Error> {
        let url = format!("{}/controller/cluster/nodes/{node_id}", self.base_url);
        let body = UpdateNodeStatusRequest {
            node: UpdateNodeBody {
                node_id: node_id.to_string(),
                status: status.as_api_str().to_string(),
            },
        };
        debug!(
            url = %url,
            node_id = %node_id,
            target_status = %status.as_api_str(),
            request_body = ?body,
            "Updating NiFi node status"
        );
        let resp = self
            .http
            .put(&url)
            .bearer_auth(&self.bearer_token)
            .json(&body)
            .send()
            .await
            .context(UpdateNodeStatusSnafu {
                node_id: node_id.to_string(),
                target_status: status.as_api_str().to_string(),
            })?;

        if !resp.status().is_success() {
            let (http_status, resp_body) = read_error_body(resp).await;
            warn!(
                http_status,
                node_id = %node_id,
                target_status = %status.as_api_str(),
                response_body = %resp_body,
                "NiFi node status update failed"
            );
            return UpdateNodeStatusHttpSnafu {
                status: http_status,
                node_id: node_id.to_string(),
                target_status: status.as_api_str().to_string(),
                body: resp_body,
            }
            .fail();
        }

        debug!(node_id = %node_id, target_status = %status.as_api_str(), "NiFi node status updated successfully");
        Ok(())
    }

    /// Delete a node from the NiFi cluster.
    ///
    /// # Parameters
    ///
    /// - `node_id`: The NiFi node identifier (UUID) to remove from the cluster.
    pub async fn delete_node(&self, node_id: &str) -> Result<(), Error> {
        let url = format!("{}/controller/cluster/nodes/{node_id}", self.base_url);
        debug!(url = %url, node_id = %node_id, "Deleting NiFi node from cluster");
        let resp = self
            .http
            .delete(&url)
            .bearer_auth(&self.bearer_token)
            .send()
            .await
            .context(DeleteNodeSnafu {
                node_id: node_id.to_string(),
            })?;

        if !resp.status().is_success() {
            let (http_status, resp_body) = read_error_body(resp).await;
            warn!(
                http_status,
                node_id = %node_id,
                response_body = %resp_body,
                "NiFi node deletion failed"
            );
            return DeleteNodeStatusSnafu {
                status: http_status,
                node_id: node_id.to_string(),
                body: resp_body,
            }
            .fail();
        }

        debug!(node_id = %node_id, "NiFi node deleted successfully");
        Ok(())
    }
}
