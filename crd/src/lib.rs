use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stackable_operator::label_selector::schema;
use stackable_operator::Crd;
use std::collections::HashMap;

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "nifi.stackable.tech",
    version = "v1",
    kind = "NiFiCluster",
    shortname = "nifi",
    namespaced
)]
#[kube(status = "NiFiStatus")]
#[serde(rename_all = "camelCase")]
pub struct NiFiSpec {
    pub version: NiFiVersion,
    pub zookeeper_connect_string: Option<String>,
    pub nodes: RoleGroup<NiFiConfig>,
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
pub enum NiFiVersion {
    #[serde(rename = "1.13.2")]
    #[strum(serialize = "1.13.2")]
    v1_13_2,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct NiFiStatus {}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleGroup<T> {
    pub selectors: HashMap<String, SelectorAndConfig<T>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectorAndConfig<T> {
    pub instances: u16,
    pub instances_per_node: u8,
    pub config: T,
    #[schemars(schema_with = "schema")]
    pub selector: Option<LabelSelector>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NiFiConfig {
    pub port: Option<u16>,
}

impl Crd for NiFiCluster {
    const RESOURCE_NAME: &'static str = "nificlusters.nifi.stackable.tech";
    const CRD_DEFINITION: &'static str = include_str!("../../deploy/crd/server.nifi.crd.yaml");
}
