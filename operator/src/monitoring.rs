use crate::{CONTAINER_NAME, HTTP_PORT_NAME};
use k8s_openapi::api::core::v1::{Container, Pod};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_nifi_crd::NifiVersion;
use std::convert::TryFrom;
use tracing::field::debug;
use tracing::warn;

const PROMETHEUS_REPORTING_TASK_NAME: &str = "StackablePrometheusReportingTask";

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTask {
    pub revision: ReportingTaskRevision,
    pub id: Option<String>,
    pub disconnected_node_acknowledged: bool,
    pub component: Option<ReportingTaskComponent>,
    pub state: Option<ReportingTaskState>,
    pub status: Option<ReportingTaskStatus>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTaskRevision {
    pub client_id: String,
    pub version: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ReportingTaskComponent {
    #[serde(rename = "type")]
    pub typ: String,
    pub name: String,
    pub bundle: ReportingTaskComponentBundle,
    pub properties: ReportingTaskComponentProperties,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTaskComponentBundle {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
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
    pub ssl_context: String,
    #[serde(rename = "prometheus-reporting-task-client-auth")]
    pub client_auth: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportingTaskStatus {
    pub run_status: ReportingTaskState,
    pub validation_status: ReportingTaskValidation,
    pub active_thread_count: usize,
}

#[derive(
    Clone,
    Debug,
    Deserialize,
    JsonSchema,
    Serialize,
    strum_macros::Display,
    strum_macros::EnumString,
)]
pub enum ReportingTaskState {
    #[serde(rename = "RUNNING")]
    #[strum(serialize = "RUNNING")]
    Running,
    #[serde(rename = "STOPPED")]
    #[strum(serialize = "STOPPED")]
    Stopped,
}

#[derive(
    Clone,
    Debug,
    Deserialize,
    JsonSchema,
    Serialize,
    strum_macros::Display,
    strum_macros::EnumString,
)]
pub enum ReportingTaskValidation {
    #[serde(rename = "VALID")]
    #[strum(serialize = "VALID")]
    Valid,
}

pub struct MonitoringStatus {
    pub client: reqwest::Client,
    pub headers: HeaderMap,
    pub reporting_task_id: Option<String>,
}

impl MonitoringStatus {
    pub fn new(client: reqwest::Client) -> Self {
        let headers: Vec<(&'static str, &str)> = vec![
            ("Accept-Encoding", "gzip, deflate, br"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/javascript, */*; q=0.01"),
        ];

        let mut header_map = HeaderMap::new();

        for (key, value) in headers {
            match HeaderValue::from_str(value) {
                Ok(val) => {
                    header_map.insert(key, val);
                }
                Err(err) => {
                    warn!(
                        "Invalid header item [{} -> {}] for Monitoring requests: {}",
                        key,
                        value,
                        err.to_string()
                    );
                }
            }
        }

        MonitoringStatus {
            client,
            headers: header_map,
            reporting_task_id: None,
        }
    }

    pub async fn reporting_task_ids(&self, pods: &[Pod]) -> Vec<(String, u16, String)> {
        let mut ids = vec![];

        for pod in pods {
            // 1) retrieve node_name
            let spec = match &pod.spec {
                None => {
                    warn!(
                        "Pod [{}] does not have any spec. Skipping...",
                        pod.metadata.name.as_ref().unwrap()
                    );
                    continue;
                }
                Some(pod_spec) => pod_spec,
            };

            let node_name = match &spec.node_name {
                None => {
                    warn!(
                        "Pod [{}] does not have any node_name set. Skipping...",
                        pod.metadata.name.as_ref().unwrap()
                    );
                    continue;
                }
                Some(name) => name,
            };

            // 2) retrieve "http" port from container ports
            let http_port = match self.http_container_port(
                spec.containers.as_slice(),
                CONTAINER_NAME,
                HTTP_PORT_NAME,
            ) {
                None => {
                    warn!(
                        "No container_port [{}] found in container [{}].",
                        HTTP_PORT_NAME, CONTAINER_NAME
                    );
                    continue;
                }
                Some(port) => port,
            };

            let url = &format!("{}:{}/nifi-api/flow/reporting-tasks", node_name, http_port);

            // 3) query GET <node_name>:<port>/nifi-api/flow/reporting-tasks
            match self.get(url).await {
                Err(err) => {
                    warn!("Error for get request: {}", err.to_string());
                    continue;
                }
                Ok(res) => {
                    // 4) check for Stackable prometheus_reporting_tasks and get id and status (Running / stopped)
                    let task = res.json::<ReportingTask>().await.unwrap();

                    let reporting_task_id = match task.id {
                        None => {
                            warn!("Retrieved no id from [{}]", url);
                            continue;
                        }
                        Some(id) => {
                            warn!("Retrieved id [{}] from [{}]", id, url);
                            id
                        }
                    };

                    // 5) check if configured port (in nifi) equals the container port (in pod)
                    if let Some(ReportingTaskComponent { properties, .. }) = task.component {
                        if properties.metrics_endpoint_port != http_port.to_string() {
                            warn!("ReportingTask [{}] metrics port [{}] does not match container '{}' port [{}] ", 
                                reporting_task_id, properties.metrics_endpoint_port, HTTP_PORT_NAME, http_port);
                            continue;
                        }
                    }

                    ids.push((node_name.clone(), http_port, reporting_task_id.to_string()));
                }
            };
        }

        // 6) return list of task ids
        ids
    }

    pub async fn create_reporting_task(
        &self,
        node_name: &str,
        rest_port: &str,
        metrics_port: &str,
        version: NifiVersion,
        url: &str,
    ) {
        let task = ReportingTask {
            revision: ReportingTaskRevision {
                client_id: "1".to_string(),
                version: 0,
            },
            id: None,
            disconnected_node_acknowledged: false,
            component: Some(ReportingTaskComponent {
                typ: "org.apache.nifi.reporting.prometheus.PrometheusReportingTask".to_string(),
                name: "StackablePrometheusReportingTask".to_string(),
                bundle: ReportingTaskComponentBundle {
                    group: "org.apache.nifi".to_string(),
                    artifact: "nifi-prometheus-nar".to_string(),
                    version: version.to_string(),
                },
                properties: ReportingTaskComponentProperties {
                    metrics_endpoint_port: metrics_port.to_string(),
                    instance_id: "${hostname(true)}".to_string(),
                    metrics_strategy: "All Components".to_string(),
                    send_jvm: "true".to_string(),
                    ssl_context: "null".to_string(),
                    client_auth: "No Authentication".to_string(),
                },
            }),
            state: None,
            status: None,
        };

        let url = &format!(
            "{}:{}/nifi-api/controller/reporting-tasks",
            node_name, rest_port
        );

        let body = serde_json::json!(&task);
        let res = self.post(url, &body.to_string()).await;
    }

    pub fn set_reporting_task_status(&self, task_id: &str) {}

    async fn get(&self, url: &str) -> Result<Response, reqwest::Error> {
        self.client
            .get(url)
            .headers(self.headers.clone())
            .send()
            .await
    }

    async fn post(&self, url: &str, body: &String) -> Result<Response, reqwest::Error> {
        self.client
            .post(url)
            .headers(self.headers.clone())
            .json(body)
            .send()
            .await
    }

    fn http_container_port(
        &self,
        containers: &[Container],
        container_name: &str,
        http_port_name: &str,
    ) -> Option<u16> {
        for container in containers {
            if container.name != container_name {
                continue;
            }

            for ports in &container.ports {
                if ports.name.as_deref() == Some(http_port_name) {
                    return u16::try_from(ports.container_port).ok();
                }
            }
        }

        None
    }
}
