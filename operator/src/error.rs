use std::num::ParseIntError;

#[derive(Debug, thiserror::Error)]
pub enum NifiError {
    #[error("Kubernetes reported error: {source}")]
    KubeError {
        #[from]
        source: kube::Error,
    },

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

    #[error("Error from ZooKeeper: {source}")]
    ZookeeperError {
        #[from]
        source: stackable_zookeeper_crd::error::Error,
    },

    #[error("Invalid Configmap. No name found which is required to query the ConfigMap.")]
    InvalidConfigMap,
}
