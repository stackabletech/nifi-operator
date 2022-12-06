pub mod authentication;

use crate::authentication::NifiAuthenticationConfig;

use serde::{Deserialize, Serialize};
use snafu::{OptionExt, Snafu};
use stackable_operator::{
    commons::{
        product_image_selection::ProductImage,
        resources::{
            CpuLimitsFragment, MemoryLimitsFragment, NoRuntimeLimits, NoRuntimeLimitsFragment,
            PvcConfig, PvcConfigFragment, ResourcesFragment,
        },
    },
    config::{
        fragment::Fragment,
        merge::{Atomic, Merge},
    },
    k8s_openapi::apimachinery::pkg::api::resource::Quantity,
    kube::{runtime::reflector::ObjectRef, CustomResource},
    product_config_utils::{ConfigError, Configuration},
    role_utils::{Role, RoleGroupRef},
    schemars::{self, JsonSchema},
};
use std::collections::BTreeMap;

pub const APP_NAME: &str = "nifi";

pub const HTTPS_PORT_NAME: &str = "https";
pub const HTTPS_PORT: u16 = 8443;
pub const PROTOCOL_PORT_NAME: &str = "protocol";
pub const PROTOCOL_PORT: u16 = 9088;
pub const BALANCE_PORT_NAME: &str = "balance";
pub const BALANCE_PORT: u16 = 6243;
pub const METRICS_PORT_NAME: &str = "metrics";
pub const METRICS_PORT: u16 = 8081;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object has no namespace associated"))]
    NoNamespace,
}

#[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[kube(
    group = "nifi.stackable.tech",
    version = "v1alpha1",
    kind = "NifiCluster",
    shortname = "nifi",
    status = "NifiStatus",
    namespaced,
    crates(
        kube_core = "stackable_operator::kube::core",
        k8s_openapi = "stackable_operator::k8s_openapi",
        schemars = "stackable_operator::schemars"
    )
)]
#[serde(rename_all = "camelCase")]
pub struct NifiSpec {
    /// Emergency stop button, if `true` then all pods are stopped without affecting configuration (as setting `replicas` to `0` would)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<bool>,
    /// The NiFi image to use
    pub image: ProductImage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Available NiFi roles
    pub nodes: Option<Role<NifiConfig>>,
    /// The reference to the ZooKeeper cluster
    pub zookeeper_config_map_name: String,
    /// Global Nifi config for e.g. authentication or sensitive properties
    pub config: NifiGlobalConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiGlobalConfig {
    /// A reference to a Secret containing username/password for the initial admin user
    pub authentication: NifiAuthenticationConfig,
    /// Configuration options for how NiFi encrypts sensitive properties on disk
    pub sensitive_properties: NifiSensitivePropertiesConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiSensitivePropertiesConfig {
    pub key_secret: String,
    pub algorithm: Option<NifiSensitiveKeyAlgorithm>,
    #[serde(default)]
    pub auto_generate: bool,
}

#[derive(strum::Display, Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiSensitiveKeyAlgorithm {
    #[strum(serialize = "NIFI_ARGON2_AES_GCM_128")]
    NifiArgon2AesGcm128,
    #[strum(serialize = "NIFI_ARGON2_AES_GCM_256")]
    NifiArgon2AesGcm256,
    #[strum(serialize = "NIFI_BCRYPT_AES_GCM_128")]
    NifiBcryptAesGcm128,
    #[strum(serialize = "NIFI_BCRYPT_AES_GCM_256")]
    NifiBcryptAesGcm256,
    #[strum(serialize = "NIFI_PBKDF2_AES_GCM_128")]
    NifiPbkdf2AesGcm128,
    #[strum(serialize = "NIFI_PBKDF2_AES_GCM_256")]
    NifiPbkdf2AesGcm256,
    #[strum(serialize = "NIFI_SCRYPT_AES_GCM_128")]
    NifiScryptAesGcm128,
    #[strum(serialize = "NIFI_SCRYPT_AES_GCM_256")]
    NifiScryptAesGcm256,
}

impl Default for NifiSensitiveKeyAlgorithm {
    fn default() -> Self {
        Self::NifiArgon2AesGcm256
    }
}

#[derive(strum::Display, Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StoreType {
    #[strum(serialize = "JKS")]
    JKS,
    #[strum(serialize = "PKCS12")]
    PKCS12,
}

impl Default for StoreType {
    fn default() -> Self {
        Self::JKS
    }
}

#[derive(strum::Display)]
#[strum(serialize_all = "camelCase")]
pub enum NifiRole {
    #[strum(serialize = "node")]
    Node,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, Serialize)]
pub struct NifiStatus {
    pub deployed_version: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiConfig {
    pub log: Option<NifiLogConfig>,
    pub resources: Option<ResourcesFragment<NifiStorageConfig, NoRuntimeLimits>>,
}

impl NifiConfig {
    pub const NIFI_SENSITIVE_PROPS_KEY: &'static str = "NIFI_SENSITIVE_PROPS_KEY";

    pub fn default_resources() -> ResourcesFragment<NifiStorageConfig, NoRuntimeLimits> {
        ResourcesFragment {
            memory: MemoryLimitsFragment {
                limit: Some(Quantity("2Gi".to_string())),
                runtime_limits: NoRuntimeLimitsFragment {},
            },
            cpu: CpuLimitsFragment {
                min: Some(Quantity("500m".to_string())),
                max: Some(Quantity("4".to_string())),
            },
            storage: NifiStorageConfigFragment {
                flowfile_repo: PvcConfigFragment {
                    capacity: Some(Quantity("2Gi".to_string())),
                    storage_class: None,
                    selectors: None,
                },
                provenance_repo: PvcConfigFragment {
                    capacity: Some(Quantity("2Gi".to_string())),
                    storage_class: None,
                    selectors: None,
                },
                database_repo: PvcConfigFragment {
                    capacity: Some(Quantity("2Gi".to_string())),
                    storage_class: None,
                    selectors: None,
                },
                content_repo: PvcConfigFragment {
                    capacity: Some(Quantity("2Gi".to_string())),
                    storage_class: None,
                    selectors: None,
                },
                state_repo: PvcConfigFragment {
                    capacity: Some(Quantity("2Gi".to_string())),
                    storage_class: None,
                    selectors: None,
                },
            },
        }
    }
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
        Ok(BTreeMap::new())
    }
}

#[derive(Clone, Debug, Default, Merge, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiLogConfig {
    pub root_log_level: Option<LogLevel>,
}

impl Atomic for LogLevel {}

#[derive(strum::Display, Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum LogLevel {
    DEBUG,
    INFO,
    WARN,
    ERROR,
    FATAL,
}

#[derive(Clone, Debug, Default, JsonSchema, PartialEq, Fragment)]
#[fragment_attrs(
    derive(
        Clone,
        Debug,
        Default,
        Deserialize,
        Merge,
        JsonSchema,
        PartialEq,
        Serialize
    ),
    serde(rename_all = "camelCase")
)]
pub struct NifiStorageConfig {
    #[fragment_attrs(serde(default))]
    pub flowfile_repo: PvcConfig,
    #[fragment_attrs(serde(default))]
    pub provenance_repo: PvcConfig,
    #[fragment_attrs(serde(default))]
    pub database_repo: PvcConfig,
    #[fragment_attrs(serde(default))]
    pub content_repo: PvcConfig,
    #[fragment_attrs(serde(default))]
    pub state_repo: PvcConfig,
}

impl NifiCluster {
    /// The name of the role-level load-balanced Kubernetes `Service`
    pub fn node_role_service_name(&self) -> Option<String> {
        self.metadata.name.clone()
    }

    /// The fully-qualified domain name of the role-level load-balanced Kubernetes `Service`
    pub fn node_role_service_fqdn(&self) -> Option<String> {
        Some(format!(
            "{}.{}.svc.cluster.local",
            self.node_role_service_name()?,
            self.metadata.namespace.as_ref()?
        ))
    }

    /// Metadata about a metastore rolegroup
    pub fn node_rolegroup_ref(&self, group_name: impl Into<String>) -> RoleGroupRef<NifiCluster> {
        RoleGroupRef {
            cluster: ObjectRef::from_obj(self),
            role: NifiRole::Node.to_string(),
            role_group: group_name.into(),
        }
    }

    /// List all pods expected to form the cluster
    ///
    /// We try to predict the pods here rather than looking at the current cluster state in order to
    /// avoid instance churn.
    pub fn pods(&self) -> Result<impl Iterator<Item = PodRef> + '_, Error> {
        let ns = self.metadata.namespace.clone().context(NoNamespaceSnafu)?;
        Ok(self
            .spec
            .nodes
            .iter()
            .flat_map(|role| &role.role_groups)
            // Order rolegroups consistently, to avoid spurious downstream rewrites
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .flat_map(move |(rolegroup_name, rolegroup)| {
                let rolegroup_ref = self.node_rolegroup_ref(rolegroup_name);
                let ns = ns.clone();
                (0..rolegroup.replicas.unwrap_or(0)).map(move |i| PodRef {
                    namespace: ns.clone(),
                    role_group_service_name: rolegroup_ref.object_name(),
                    pod_name: format!("{}-{}", rolegroup_ref.object_name(), i),
                })
            }))
    }
}

/// Reference to a single `Pod` that is a component of a [`NifiCluster`]
/// Used for service discovery.
// TODO: this should move to operator-rs
pub struct PodRef {
    pub namespace: String,
    pub role_group_service_name: String,
    pub pod_name: String,
}

impl PodRef {
    pub fn fqdn(&self) -> String {
        format!(
            "{}.{}.{}.svc.cluster.local",
            self.pod_name, self.role_group_service_name, self.namespace
        )
    }
}
