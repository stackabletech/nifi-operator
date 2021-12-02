use semver::Version;
use serde::{Deserialize, Serialize};
use stackable_operator::identity::PodToNodeMapping;
use stackable_operator::k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use stackable_operator::kube::CustomResource;
use stackable_operator::product_config_utils::{ConfigError, Configuration};
use stackable_operator::role_utils::Role;
use stackable_operator::schemars::{self, JsonSchema};
use stackable_operator::status::{Conditions, Status, Versioned};
use stackable_operator::versioning::{ProductVersion, Versioning, VersioningState};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use strum_macros::Display;
use strum_macros::EnumIter;

pub const APP_NAME: &str = "nifi";
pub const MANAGED_BY: &str = "nifi-operator";

pub const NIFI_WEB_HTTP_PORT: &str = "nifi.web.http.port";
pub const NIFI_CLUSTER_NODE_PROTOCOL_PORT: &str = "nifi.cluster.node.protocol.port";
pub const NIFI_CLUSTER_LOAD_BALANCE_PORT: &str = "nifi.cluster.load.balance.port";
pub const NIFI_CLUSTER_METRICS_PORT: &str = "metricsPort";

pub const NIFI_SENSITIVE_PROPERTY_KEY: &str = "NIFI_SENSITIVE_PROPERTY_KEY";

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, Serialize)]
#[kube(
    group = "nifi.stackable.tech",
    version = "v1alpha1",
    kind = "NifiCluster",
    shortname = "nifi",
    status = "NifiStatus",
    namespaced,
    kube_core = "stackable_operator::kube::core",
    k8s_openapi = "stackable_operator::k8s_openapi",
    schemars = "stackable_operator::schemars"
)]
#[kube()]
#[serde(rename_all = "camelCase")]
pub struct NifiSpec {
    pub metrics_port: Option<u16>,
    pub nodes: Role<NifiConfig>,
    pub version: NifiVersion,
    pub zookeeper_reference: stackable_zookeeper_crd::util::ZookeeperReference,
}

impl Status<NifiStatus> for NifiCluster {
    fn status(&self) -> &Option<NifiStatus> {
        &self.status
    }
    fn status_mut(&mut self) -> &mut Option<NifiStatus> {
        &mut self.status
    }
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
    #[serde(rename = "1.15.0")]
    #[strum(serialize = "1.15.0")]
    v1_15_0,
}

impl Versioning for NifiVersion {
    fn versioning_state(&self, other: &Self) -> VersioningState {
        let from_version = match Version::parse(&self.to_string()) {
            Ok(v) => v,
            Err(e) => {
                return VersioningState::Invalid(format!(
                    "Could not parse [{}] to SemVer: {}",
                    self.to_string(),
                    e.to_string()
                ))
            }
        };

        let to_version = match Version::parse(&other.to_string()) {
            Ok(v) => v,
            Err(e) => {
                return VersioningState::Invalid(format!(
                    "Could not parse [{}] to SemVer: {}",
                    other.to_string(),
                    e.to_string()
                ))
            }
        };

        match to_version.cmp(&from_version) {
            Ordering::Greater => VersioningState::ValidUpgrade,
            Ordering::Less => VersioningState::ValidDowngrade,
            Ordering::Equal => VersioningState::NoOp,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct NifiStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<ProductVersion<NifiVersion>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<PodToNodeMapping>,
}

impl Versioned<NifiVersion> for NifiStatus {
    fn version(&self) -> &Option<ProductVersion<NifiVersion>> {
        &self.version
    }
    fn version_mut(&mut self) -> &mut Option<ProductVersion<NifiVersion>> {
        &mut self.version
    }
}

impl Conditions for NifiStatus {
    fn conditions(&self) -> &[Condition] {
        self.conditions.as_slice()
    }
    fn conditions_mut(&mut self) -> &mut Vec<Condition> {
        &mut self.conditions
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiConfig {
    pub http_port: Option<u16>,
    pub protocol_port: Option<u16>,
    pub load_balance_port: Option<u16>,
    pub sensitive_property_key_secret: String,
}

impl Configuration for NifiConfig {
    type Configurable = NifiCluster;

    fn compute_env(
        &self,
        resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ConfigError> {
        let mut result = BTreeMap::new();
        if let Some(metrics_port) = &resource.spec.metrics_port {
            result.insert(
                NIFI_CLUSTER_METRICS_PORT.to_string(),
                Some(metrics_port.to_string()),
            );
        }
        result.insert(
            NIFI_SENSITIVE_PROPERTY_KEY.to_string(),
            Some(self.sensitive_property_key_secret.to_string())
        );
        Ok(result)
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

#[cfg(test)]
mod tests {
    use crate::NifiVersion;
    use stackable_operator::versioning::{Versioning, VersioningState};
    use std::str::FromStr;

    #[test]
    fn test_zookeeper_version_versioning() {
        assert_eq!(
            NifiVersion::v1_15_0.versioning_state(&NifiVersion::v1_15_0),
            VersioningState::NoOp
        );
    }

    #[test]
    fn test_version_conversion() {
        NifiVersion::from_str("1.15.0").unwrap();
        NifiVersion::from_str("1.2.3").unwrap_err();
    }
}
