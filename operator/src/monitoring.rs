use k8s_openapi::api::core::v1::Pod;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use tracing::{debug, info, trace, warn};

const PROMETHEUS_REPORTING_TASK_NAME: &str = "StackablePrometheusReportingTask";

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

    pub fn reporting_task_ids(&self, pods: &[&Pod]) {
        // 1) retrieve node_name
        // 2) retrieve "http" port from container ports
        // 3) query GET <node_name>:<port>/nifi-api/flow/reporting-tasks
        // 4) check for Stackable prometheus_reporting_tasks and get id and status (Running / stopped)
        // 5) check if configured port (in nifi) equals the container port (in pod)
        // 6) return list of task ids
    }

    pub fn create_reporting_task(&self, port: &str) {}

    pub fn set_reporting_task_status(&self, task_id: &str) {}
}
