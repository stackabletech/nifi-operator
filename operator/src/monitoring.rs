use crate::{NifiError, METRICS_PORT_NAME};
use crate::{CONTAINER_NAME, HTTP_PORT_NAME};
use k8s_openapi::api::core::v1::{Container, Pod};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use stackable_operator::reconcile::ReconcileFunctionAction;
use std::convert::TryFrom;
use std::time::Duration;
use tracing::{info, warn};

const PROMETHEUS_REPORTING_TASK_NAME: &str = "StackablePrometheusReportingTask";
const PROMETHEUS_REPORTING_TASK_TYPE: &str =
    "org.apache.nifi.reporting.prometheus.PrometheusReportingTask";

const PROMETHEUS_REPORTING_TASK_BUNDLE_GROUP: &str = "org.apache.nifi";
const PROMETHEUS_REPORTING_TASK_BUNDLE_ARTIFACT: &str = "nifi-prometheus-nar";

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
            ("Accept", "application/json"),
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

    pub async fn reporting_task_ids(
        &self,
        pods: &[Pod],
        version: &str,
    ) -> Result<ReconcileFunctionAction, NifiError> {
        let node_names_and_http_container_ports_and_uids =
            self.node_names_and_container_ports_and_uids(pods)?;

        for (node_name, http_port, metric_port, uid) in node_names_and_http_container_ports_and_uids
        {
            let url = &format!(
                "http://{}:{}/nifi-api/flow/reporting-tasks",
                node_name, http_port
            );

            let tasks: ReportingTasks = self.client.get(url).send().await?.json().await?;

            // find reporting task for pod
            let task = self.find_task_for_pod(&tasks, &uid, version);

            // if we have a task, check if state is running, otherwise set to running
            if let Some(my_task) = task {
                let task_id = my_task.id.as_deref().unwrap_or("<no-id-found>");
                // check if configured port (in nifi) equals the container port (in pod)
                if let Some(ReportingTaskComponent { properties, .. }) = &my_task.component {
                    if properties.metrics_endpoint_port != metric_port {
                        warn!("ReportingTask [{}] metrics port [{}] does not match container '{}' port [{}] ",
                                    task_id, properties.metrics_endpoint_port, METRICS_PORT_NAME, metric_port);
                        // TODO: what to do here?
                        continue;
                    }
                }

                if let Some(status) = &my_task.status {
                    if status.run_status == ReportingTaskState::Stopped {
                        info!(
                            "Task [{}] has status [{}]. Starting it...",
                            task_id,
                            ReportingTaskState::Stopped.to_string()
                        );

                        self.reporting_task_status(
                            &node_name,
                            &http_port,
                            task_id,
                            &uid,
                            ReportingTaskState::Running,
                        )
                        .await?;
                    }
                }
            }
            // no task available yet -> create
            else {
                info!(
                    "Found missing monitoring task for [{}:{}]. Creating it..",
                    node_name, http_port
                );

                self.create_reporting_task(&node_name, &http_port, &uid, &metric_port, version)
                    .await?;
                // we need a requeue after creation to start the task
                return Ok(ReconcileFunctionAction::Requeue(Duration::from_secs(5)));
            }
        }

        Ok(ReconcileFunctionAction::Continue)
    }

    pub async fn create_reporting_task(
        &self,
        node_name: &str,
        http_port: &str,
        uid: &str,
        metrics_port: &str,
        version: &str,
    ) -> Result<Response, reqwest::Error> {
        let task = ReportingTask {
            revision: ReportingTaskRevision {
                client_id: Some(uid.to_string()),
                version: 0,
            },
            id: None,
            disconnected_node_acknowledged: Some(false),
            component: Some(ReportingTaskComponent {
                typ: PROMETHEUS_REPORTING_TASK_TYPE.to_string(),
                name: build_task_name(uid),
                bundle: ReportingTaskComponentBundle {
                    group: PROMETHEUS_REPORTING_TASK_BUNDLE_GROUP.to_string(),
                    artifact: PROMETHEUS_REPORTING_TASK_BUNDLE_ARTIFACT.to_string(),
                    version: version.to_string(),
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

        let url = &format!(
            "http://{}:{}/nifi-api/controller/reporting-tasks",
            node_name, http_port
        );

        self.post(url, &task).await
    }

    pub async fn reporting_task_status(
        &self,
        node_name: &str,
        http_port: &str,
        task_id: &str,
        uid: &str,
        state: ReportingTaskState,
    ) -> Result<Response, reqwest::Error> {
        let new_task = ReportingTask {
            revision: ReportingTaskRevision {
                version: 0,
                client_id: Some(uid.to_string()),
            },
            disconnected_node_acknowledged: Some(true),
            state: Some(state),
            id: None,
            component: None,
            status: None,
        };

        self.put(
            &format!(
                "http://{}:{}/nifi-api/reporting-tasks/{}/run-status",
                node_name, http_port, task_id
            ),
            &new_task,
        )
        .await
    }

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

    fn container_port(
        &self,
        containers: &[Container],
        container_name: &str,
        port_name: &str,
    ) -> Option<u16> {
        for container in containers {
            if container.name != container_name {
                continue;
            }

            for ports in &container.ports {
                if ports.name.as_deref() == Some(port_name) {
                    return u16::try_from(ports.container_port).ok();
                }
            }
        }
        None
    }

    fn node_names_and_container_ports_and_uids(
        &self,
        pods: &[Pod],
    ) -> Result<Vec<(String, String, String, String)>, NifiError> {
        let mut result = vec![];

        for pod in pods {
            let pod_name = pod.metadata.name.as_ref().unwrap();

            let spec = match &pod.spec {
                None => {
                    warn!("Pod [{}] does not have any spec. Skipping...", pod_name);
                    continue;
                }
                Some(pod_spec) => pod_spec,
            };

            let node_name = match &spec.node_name {
                None => {
                    warn!(
                        "Pod [{}] does not have any node_name set. Skipping...",
                        pod_name
                    );
                    continue;
                }
                Some(name) => name,
            };

            let http_port = match self.container_port(
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

            let metric_port = match self.container_port(
                spec.containers.as_slice(),
                CONTAINER_NAME,
                METRICS_PORT_NAME,
            ) {
                None => {
                    warn!(
                        "No container_port [{}] found in container [{}].",
                        METRICS_PORT_NAME, CONTAINER_NAME
                    );
                    continue;
                }
                Some(port) => port,
            };

            let uid = match &pod.metadata.uid {
                None => {
                    warn!("No uid found in pod [{}].", pod_name);
                    continue;
                }
                Some(port) => port,
            };

            result.push((
                node_name.clone(),
                http_port.to_string(),
                metric_port.to_string(),
                uid.clone(),
            ));
        }

        Ok(result)
    }

    fn find_task_for_pod<'a>(
        &self,
        tasks: &'a ReportingTasks,
        uid: &str,
        nifi_version: &str,
    ) -> Option<&'a ReportingTask> {
        for task in &tasks.reporting_tasks {
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
                if typ != PROMETHEUS_REPORTING_TASK_TYPE && name != &build_task_name(uid) {
                    continue;
                }

                if group != PROMETHEUS_REPORTING_TASK_BUNDLE_GROUP
                    && artifact != PROMETHEUS_REPORTING_TASK_BUNDLE_ARTIFACT
                    && version != nifi_version
                {
                    continue;
                }

                // task type and name, bundle group, artifact and version match
                return Some(task);
            }
        }

        None
    }
}

fn build_task_name(uid: &str) -> String {
    format!("{}-{}", PROMETHEUS_REPORTING_TASK_NAME, uid)
}
