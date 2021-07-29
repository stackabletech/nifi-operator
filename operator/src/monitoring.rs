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

    pub fn reporting_task_ids() {}
}
