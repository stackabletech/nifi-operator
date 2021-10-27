use crate::monitoring;
use stackable_operator::kube;
use std::num::ParseIntError;

#[allow(clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum NifiError {
    #[error("Kubernetes reported error: {source}")]
    KubeError {
        #[from]
        source: kube::Error,
    },

    #[error("NiFi monitoring reported error: {source}")]
    NifiMonitoringError {
        #[from]
        source: monitoring::NifiMonitoringError,
    },

    #[error(
        "ConfigMap of type [{cm_type}] is for pod with generate_name [{pod_name}] is missing."
    )]
    MissingConfigMapError {
        cm_type: &'static str,
        pod_name: String,
    },

    #[error("ConfigMap of type [{cm_type}] is missing the metadata.name. Maybe the config map was not created yet?")]
    MissingConfigMapNameError { cm_type: &'static str },

    #[error("Error from Operator framework: {source}")]
    OperatorError {
        #[from]
        source: stackable_operator::error::Error,
    },

    #[error("Error from parsing: {source}")]
    ParseError {
        #[from]
        source: ParseIntError,
    },

    #[error("Reqwest reported error: {source}")]
    ReqwestError {
        #[from]
        source: reqwest::Error,
    },

    #[error("Error from serde_json: {source}")]
    SerdeError {
        #[from]
        source: serde_json::Error,
    },

    #[error("Error with ZooKeeper connection. Could not retrieve the ZooKeeper connection. This is a bug. Please open a ticket.")]
    ZookeeperConnectionInformationError,

    #[error("Error from ZooKeeper: {source}")]
    ZookeeperError {
        #[from]
        source: stackable_zookeeper_crd::error::Error,
    },
}
