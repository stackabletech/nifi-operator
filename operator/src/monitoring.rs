use crate::{CONTAINER_NAME, HTTP_PORT_NAME};
use k8s_openapi::api::core::v1::{Container, Pod};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::future::Future;
use tracing::{debug, error, warn};

const PROMETHEUS_REPORTING_TASK_NAME: &str = "StackablePrometheusReportingTask";
const PROMETHEUS_REPORTING_TASK_TYPE: &str =
    "org.apache.nifi.reporting.prometheus.PrometheusReportingTask";

const PROMETHEUS_REPORTING_TASK_BUNDLE_GROUP: &str = "org.apache.nifi";
const PROMETHEUS_REPORTING_TASK_BUNDLE_ARTIFACT: &str = "nifi-prometheus-nar";

pub const NO_TASK_ID: &str = "<no-task-id>";

#[derive(Debug, thiserror::Error)]
pub enum NifiMonitoringError {
    #[error("Missing node_name for pod [{pod_name}].")]
    PodNodeNameMissing { pod_name: String },

    #[error("Missing spec for pod [{pod_name}].")]
    PodSpecMissing { pod_name: String },

    #[error(
        "Container [{container_name}] missing required container_port [{container_port_name}]."
    )]
    PodContainerPortMissing {
        container_name: String,
        container_port_name: String,
    },

    #[error("Reqwest reported error: {source}")]
    ReqwestError {
        #[from]
        source: reqwest::Error,
    },

    #[error("Error during parsing integer: {reason}")]
    ParseIntError { reason: String },

    #[error(
        "Error while trying to connect to the NiFi cluster REST endpoints. Please check the cluster availability or write a ticket."
    )]
    NiFiRestApiUnreachable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTasks {
    pub reporting_tasks: Vec<ReportingTask>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTask {
    pub revision: ReportingTaskRevision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disconnected_node_acknowledged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<ReportingTaskComponent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<ReportingTaskState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ReportingTaskStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTaskRevision {
    pub client_id: Option<String>,
    pub version: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReportingTaskComponent {
    #[serde(rename = "type")]
    pub typ: String,
    pub name: String,
    pub bundle: ReportingTaskComponentBundle,
    pub properties: ReportingTaskComponentProperties,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTaskComponentBundle {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReportingTaskComponentProperties {
    #[serde(rename = "prometheus-reporting-task-metrics-endpoint-port")]
    pub metrics_endpoint_port: String,
    #[serde(rename = "prometheus-reporting-task-instance-id")]
    pub instance_id: String,
    #[serde(rename = "prometheus-reporting-task-metrics-strategy")]
    pub metrics_strategy: String,
    #[serde(rename = "prometheus-reporting-task-metrics-send-jvm")]
    pub send_jvm: String,
    #[serde(rename = "prometheus-reporting-task-ssl-context")]
    pub ssl_context: Option<String>,
    #[serde(rename = "prometheus-reporting-task-client-auth")]
    pub client_auth: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTaskStatus {
    pub run_status: ReportingTaskState,
    pub validation_status: ReportingTaskValidation,
    pub active_thread_count: usize,
}

#[derive(
    Clone, Debug, Deserialize, Serialize, PartialEq, strum_macros::Display, strum_macros::EnumString,
)]
pub enum ReportingTaskState {
    #[serde(rename = "RUNNING")]
    #[strum(serialize = "RUNNING")]
    Running,
    #[serde(rename = "STOPPED")]
    #[strum(serialize = "STOPPED")]
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Serialize, strum_macros::Display, strum_macros::EnumString)]
pub enum ReportingTaskValidation {
    #[serde(rename = "VALID")]
    #[strum(serialize = "VALID")]
    Valid,
}

/// Collects required information about pods. The node_name and http_port form the host address
/// for the NiFi REST API.
pub struct NifiRestEndpoint {
    pub node_name: String,
    pub http_port: u16,
}

impl NifiRestEndpoint {
    /// Return the host address containing of the node_name and the http_port.
    pub fn http_host(&self) -> String {
        format!("http://{}:{}", self.node_name, self.http_port)
    }

    /// Url to the NiFi flow reporting tasks.
    pub fn list_reporting_tasks_url(&self) -> String {
        format!("{}/nifi-api/flow/reporting-tasks", self.http_host())
    }

    /// Url to the NiFi controller reporting tasks.
    pub fn create_reporting_task_url(&self) -> String {
        format!("{}/nifi-api/controller/reporting-tasks", self.http_host())
    }

    /// Url to the reporting-tasks api.
    pub fn delete_reporting_task_url(
        &self,
        task_id: &str,
        revision: &ReportingTaskRevision,
    ) -> String {
        format!(
            "{}/nifi-api/reporting-tasks/{}?version={}&clientId={}&disconnectedNodeAcknowledged=false",
            self.http_host(),
            task_id, revision.version, revision.client_id.as_deref().unwrap_or("1")
        )
    }

    /// Url to change the status of an existing task.
    pub fn update_reporting_task_status_url(&self, task_id: &str) -> String {
        format!(
            "{}/nifi-api/reporting-tasks/{}/run-status",
            self.http_host(),
            task_id
        )
    }
}

pub struct NifiRestClient {
    pub client: reqwest::Client,
    pub headers: HeaderMap,
}

impl NifiRestClient {
    pub fn new(client: reqwest::Client) -> Self {
        let headers: Vec<(&'static str, &str)> = vec![
            ("Accept-Encoding", "gzip, deflate, br"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
        ];

        let mut header_map = HeaderMap::new();

        for (key, value) in headers {
            match HeaderValue::from_str(value) {
                Ok(val) => {
                    header_map.insert(key, val);
                }
                Err(err) => {
                    debug!(
                        "Invalid header item [{} -> {}] for monitoring requests: {}",
                        key,
                        value,
                        err.to_string()
                    );
                }
            }
        }

        NifiRestClient {
            client,
            headers: header_map,
        }
    }

    /// Find the "StackablePrometheusReportingTask" in the NiFi cluster.
    ///
    /// # Arguments
    /// * `nifi_rest_endpoints` - Pod REST endpoints info including (node_name, http_port).
    /// * `nifi_version` - The NiFi version currently used by the operator.
    ///
    pub async fn find_reporting_task(
        &self,
        nifi_rest_endpoints: &[NifiRestEndpoint],
        nifi_version: &str,
    ) -> Result<Option<ReportingTask>, NifiMonitoringError> {
        let tasks = self.list_reporting_tasks(nifi_rest_endpoints).await?;
        for task in tasks {
            if let Some(ReportingTaskComponent {
                typ,
                name,
                bundle:
                    ReportingTaskComponentBundle {
                        group,
                        artifact,
                        version,
                    },
                ..
            }) = &task.component
            {
                if typ == PROMETHEUS_REPORTING_TASK_TYPE
                    && name == &build_task_name()
                    && group == PROMETHEUS_REPORTING_TASK_BUNDLE_GROUP
                    && artifact == PROMETHEUS_REPORTING_TASK_BUNDLE_ARTIFACT
                    && version == nifi_version
                {
                    // task type and name, bundle group, artifact and version match
                    return Ok(Some(task));
                }
            }
        }

        Ok(None)
    }

    /// List all available ReportingTasks from a NiFi node REST endpoint.
    /// Will try pod after pod until the first can be queried via REST.
    ///
    /// # Arguments
    /// * `nifi_rest_endpoints` - Pod REST endpoints info including (node_name, http_port).
    ///
    pub async fn list_reporting_tasks(
        &self,
        nifi_rest_endpoints: &[NifiRestEndpoint],
    ) -> Result<Vec<ReportingTask>, NifiMonitoringError> {
        // We try all pods here in case there are network or other problems
        // and the first pod we encounter is not reachable.
        // We return however after the first pod that accepts requests.
        // ReportingTasks are shared via all pods/nifi nodes so one responding pod is sufficient.
        for info in nifi_rest_endpoints {
            let url = info.list_reporting_tasks_url();

            match self
                .client
                .get(&url)
                .send()
                .await?
                .json::<ReportingTasks>()
                .await
            {
                Ok(tasks) => {
                    return Ok(tasks.reporting_tasks);
                }
                // continue here if we have more pods to check
                Err(err) => {
                    warn!(
                        "Skipping url [{}] that is currently not reachable with error: {}",
                        &url,
                        err.to_string()
                    );
                    continue;
                }
            };
        }

        Ok(Vec::new())
    }

    /// Collect a list of monitoring info for pods. Contains node_name and http_port (to make
    /// REST api calls), the metrics_port and the pod uid.
    ///
    /// # Arguments
    /// * `pods` - List of all available NiFi pods
    ///
    pub fn list_nifi_rest_endpoints(
        &self,
        pods: &[Pod],
    ) -> Result<Vec<NifiRestEndpoint>, NifiMonitoringError> {
        let mut result = vec![];
        for pod in pods {
            result.push(nifi_rest_endpoint(pod)?);
        }
        Ok(result)
    }

    /// Build and create a PrometheusReportingTask in NiFi.
    ///
    /// # Arguments
    /// * `nifi_rest_endpoint` - Pod REST endpoint info including (node_name, http_port).
    /// * `nifi_version` - The NiFi version currently used by the operator.
    ///
    pub async fn create_reporting_task(
        &self,
        nifi_rest_endpoint: &NifiRestEndpoint,
        metrics_port: u16,
        nifi_version: &str,
    ) -> Result<Response, reqwest::Error> {
        let task = ReportingTask {
            revision: ReportingTaskRevision {
                client_id: None,
                version: 0,
            },
            id: None,
            disconnected_node_acknowledged: Some(false),
            component: Some(ReportingTaskComponent {
                typ: PROMETHEUS_REPORTING_TASK_TYPE.to_string(),
                name: build_task_name(),
                bundle: ReportingTaskComponentBundle {
                    group: PROMETHEUS_REPORTING_TASK_BUNDLE_GROUP.to_string(),
                    artifact: PROMETHEUS_REPORTING_TASK_BUNDLE_ARTIFACT.to_string(),
                    version: nifi_version.to_string(),
                },
                properties: ReportingTaskComponentProperties {
                    metrics_endpoint_port: metrics_port.to_string(),
                    instance_id: "${hostname(true)}".to_string(),
                    metrics_strategy: "All Components".to_string(),
                    send_jvm: "true".to_string(),
                    ssl_context: None,
                    client_auth: "No Authentication".to_string(),
                },
            }),
            state: None,
            status: None,
        };

        self.post(&nifi_rest_endpoint.create_reporting_task_url(), &task)
            .await
    }

    /// Delete a reporting_task with a certain `task_id`.
    ///
    /// # Arguments
    /// * `nifi_rest_endpoint` - Pod REST endpoint info including (node_name, http_port).
    /// * `task_id` - The PrometheusReportingTask id.
    /// * `task_revision` - The PrometheusReportingTask revision.
    ///
    pub async fn delete_reporting_task(
        &self,
        nifi_rest_endpoint: &NifiRestEndpoint,
        task_id: &str,
        task_revision: &ReportingTaskRevision,
    ) -> Result<Response, reqwest::Error> {
        self.delete(&nifi_rest_endpoint.delete_reporting_task_url(task_id, task_revision))
            .await
    }

    /// Update a reporting_task to "Stopped" or "Running".
    ///
    /// # Arguments
    /// * `nifi_rest_endpoint` - Pod REST endpoint info including (node_name, http_port).
    /// * `task_id` - The PrometheusReportingTask id.
    /// * `task_revision` - The PrometheusReportingTask revision.
    /// * `state` - The ReportingTaskState to update (STOPPED, RUNNING).
    ///
    pub async fn update_reporting_task_status(
        &self,
        nifi_rest_endpoint: &NifiRestEndpoint,
        task_id: &str,
        task_revision: &ReportingTaskRevision,
        state: ReportingTaskState,
    ) -> Result<Response, reqwest::Error> {
        let new_task = ReportingTask {
            revision: task_revision.clone(),
            disconnected_node_acknowledged: Some(false),
            state: Some(state),
            id: None,
            component: None,
            status: None,
        };

        self.put(
            &nifi_rest_endpoint.update_reporting_task_status_url(task_id),
            &new_task,
        )
        .await
    }

    /// Check if the configured [`ReportingTask`] port (in NiFi) equals the container port (in the
    /// pod). The metric port is the same for all pods / NiFi nodes.
    ///
    /// # Arguments
    /// * `metrics_port` - The provided metrics container port.
    /// * `task_component` - The [`ReportingTaskComponent`] to check
    /// * `task_id` - The PrometheusReportingTask id.
    ///
    pub fn match_metric_and_reporting_task_port(
        &self,
        metrics_port: u16,
        task_component: &Option<ReportingTaskComponent>,
    ) -> bool {
        task_component.as_ref().map_or(false, |task| {
            task.properties.metrics_endpoint_port == metrics_port.to_string()
        })
    }

    /// HTTP POST request with a body that implements `Serialize`. Headers are automatically
    /// added and the format is JSON.
    ///
    /// # Arguments
    /// * `url` - The url to perform the POST request.
    /// * `body` - Struct implementing `Serialize`.
    ///
    async fn post<T>(&self, url: &str, body: &T) -> Result<Response, reqwest::Error>
    where
        T: Serialize + ?Sized,
    {
        self.client
            .post(url)
            .headers(self.headers.clone())
            .json(body)
            .send()
            .await
    }

    /// HTTP PUT request with a body that implements `Serialize`. Headers are automatically
    /// added and the format is JSON.
    ///
    /// # Arguments
    /// * `url` - The url to perform the PUT request.
    /// * `body` - Struct implementing `Serialize`.
    ///
    async fn put<T>(&self, url: &str, body: &T) -> Result<Response, reqwest::Error>
    where
        T: Serialize + ?Sized,
    {
        self.client
            .put(url)
            .headers(self.headers.clone())
            .json(body)
            .send()
            .await
    }

    /// HTTP DELETE request. Headers are automatically added and the format is JSON.
    ///
    /// # Arguments
    /// * `url` - The url to perform the DELETE request.
    ///
    async fn delete(&self, url: &str) -> Result<Response, reqwest::Error> {
        self.client
            .delete(url)
            .headers(self.headers.clone())
            .send()
            .await
    }
}

/// Create the name of the reporting task. For now it is a fixed value. Maybe adapted later
/// with uid etc.
fn build_task_name() -> String {
    PROMETHEUS_REPORTING_TASK_NAME.to_string()
}

/// Extract [`PodMonitoringInfo`] containing required information for monitoring. Contains
/// node_name and 'container' http_port which are required to build a host address in the format
/// <node_name>:<http_port>. Additionally the actual metric_port and the uid of the pod are extracted.  
///
/// # Arguments
/// * `pod` - The pod to extract the [`PodMonitoringInfo`] from  
///
fn nifi_rest_endpoint(pod: &Pod) -> Result<NifiRestEndpoint, NifiMonitoringError> {
    let pod_name = pod.metadata.name.as_ref().unwrap();

    let spec = match &pod.spec {
        None => {
            return Err(NifiMonitoringError::PodSpecMissing {
                pod_name: pod_name.clone(),
            })
        }
        Some(pod_spec) => pod_spec,
    };

    let node_name = match &spec.node_name {
        None => {
            return Err(NifiMonitoringError::PodNodeNameMissing {
                pod_name: pod_name.clone(),
            })
        }
        Some(name) => name,
    };

    let http_port =
        find_pod_container_port(spec.containers.as_slice(), CONTAINER_NAME, HTTP_PORT_NAME)?;

    Ok(NifiRestEndpoint {
        node_name: node_name.clone(),
        http_port,
    })
}

/// Extract a container_port_number from a named container_port from a list of containers.
///
/// # Arguments
/// * `containers` - List of containers from a Pod
/// * `container_name` - The name of the container that should hold the `port_name` port.
/// * `port_name` - The name of the container_port_number we are interested in.
///
fn find_pod_container_port(
    containers: &[Container],
    container_name: &str,
    port_name: &str,
) -> Result<u16, NifiMonitoringError> {
    for container in containers {
        if container.name != container_name {
            continue;
        }

        for ports in &container.ports {
            if ports.name.as_deref() == Some(port_name) {
                return u16::try_from(ports.container_port).map_err(|err| {
                    NifiMonitoringError::ParseIntError {
                        reason: err.to_string(),
                    }
                });
            }
        }
    }

    Err(NifiMonitoringError::PodContainerPortMissing {
        container_name: container_name.to_string(),
        container_port_name: port_name.to_string(),
    })
}

/// This is a wrapper for all NiFi REST API calls like delete_reporting_task, create_reporting_task,
/// or update_reporting_task_status.
/// We iterate over the monitoring info of all pods in case of network problems or a certain
/// node is not reachable. We use the first monitoring info (host address) that is working.
pub async fn try_with_nifi_rest_endpoints<'a, F, Fut>(
    nifi_rest_endpoints: &'a [NifiRestEndpoint],
    method: F,
) -> Result<(), NifiMonitoringError>
where
    F: Fn(&'a NifiRestEndpoint) -> Fut,
    Fut: Future<Output = Result<Response, reqwest::Error>>,
{
    for info in nifi_rest_endpoints {
        match method(info).await {
            Err(err) => {
                warn!(
                    "Could not connect to NiFi REST endpoint [{}]: {}",
                    info.http_host(),
                    err.to_string()
                );
            }
            // stop if no error occurred
            Ok(_) => {
                return Ok(());
            }
        }
    }

    if nifi_rest_endpoints.is_empty() {
        Err(NifiMonitoringError::NiFiRestApiUnreachable)
    } else {
        Ok(())
    }
}
