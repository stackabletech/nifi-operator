use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::product_config_utils::{ConfigError, Configuration};
use stackable_operator::role_utils::Role;
use std::collections::BTreeMap;
use strum_macros::Display;
use strum_macros::EnumIter;

pub const APP_NAME: &str = "nifi";
pub const MANAGED_BY: &str = "nifi-operator";

pub const NIFI_WEB_HTTP_PORT: &str = "nifi.web.http.port";
pub const NIFI_CLUSTER_NODE_PROTOCOL_PORT: &str = "nifi.cluster.node.protocol.port";
pub const NIFI_CLUSTER_LOAD_BALANCE_PORT: &str = "nifi.cluster.load.balance.port";

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "nifi.stackable.tech",
    version = "v1alpha1",
    kind = "NifiCluster",
    shortname = "nifi",
    namespaced
)]
#[kube(status = "NifiStatus")]
#[serde(rename_all = "camelCase")]
pub struct NifiSpec {
    pub version: NifiVersion,
    pub zookeeper_reference: stackable_zookeeper_crd::util::ZookeeperReference,
    pub nodes: Role<NifiConfig>,
}

#[allow(non_camel_case_types)]
#[derive(
    Clone,
    Debug,
    Deserialize,
    Eq,
    Hash,
    JsonSchema,
    PartialEq,
    Serialize,
    strum_macros::Display,
    strum_macros::EnumString,
)]
pub enum NifiVersion {
    #[serde(rename = "1.13.2")]
    #[strum(serialize = "1.13.2")]
    v1_13_2,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct NifiStatus {}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiConfig {
    pub http_port: Option<u16>,
    // TODO: This has no default value, maybe remove the option?
    pub protocol_port: Option<u16>,
    pub load_balance_port: Option<u16>,
}

impl Configuration for NifiConfig {
    type Configurable = NifiCluster;

    fn compute_env(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        Ok(BTreeMap::new())
    }

    fn compute_cli(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        Ok(BTreeMap::new())
    }

    fn compute_files(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
        _file: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        let mut result = BTreeMap::new();

        if let Some(http_port) = &self.http_port {
            result.insert(NIFI_WEB_HTTP_PORT.to_string(), Some(http_port.to_string()));
        }
        if let Some(protocol_port) = &self.protocol_port {
            result.insert(
                NIFI_CLUSTER_NODE_PROTOCOL_PORT.to_string(),
                Some(protocol_port.to_string()),
            );
        }
        if let Some(load_balance_port) = &self.load_balance_port {
            result.insert(
                NIFI_CLUSTER_LOAD_BALANCE_PORT.to_string(),
                Some(load_balance_port.to_string()),
            );
        }

        Ok(result)
    }
}

#[derive(EnumIter, Debug, Display, PartialEq, Eq, Hash)]
pub enum NifiRole {
    #[strum(serialize = "node")]
    Node,
}
