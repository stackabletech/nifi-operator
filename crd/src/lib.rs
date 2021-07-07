use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::product_config_utils::{ConfigError, Configuration};
use stackable_operator::role_utils::Role;
use stackable_operator::Crd;
use std::collections::BTreeMap;
use strum_macros::Display;
use strum_macros::EnumIter;

pub const APP_NAME: &str = "nifi";
pub const MANAGED_BY: &str = "nifi-operator";

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "nifi.stackable.tech",
    version = "v1",
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
    pub node_protocol_port: Option<u16>,
    pub node_load_balancing_port: Option<u16>,
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
        let config = BTreeMap::new();
        Ok(config)
    }
}

#[derive(EnumIter, Debug, Display, PartialEq, Eq, Hash)]
pub enum NifiRole {
    Node,
}

impl Crd for NifiCluster {
    const RESOURCE_NAME: &'static str = "nificlusters.nifi.stackable.tech";
    const CRD_DEFINITION: &'static str = include_str!("../../deploy/crd/nificluster.crd.yaml");
}
