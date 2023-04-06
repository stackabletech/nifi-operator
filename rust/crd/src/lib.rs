pub mod affinity;
pub mod authentication;

use crate::authentication::NifiAuthenticationConfig;
use std::collections::BTreeMap;

use affinity::get_affinity;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::k8s_openapi::api::core::v1::Volume;
use stackable_operator::{
    commons::{
        affinity::StackableAffinity,
        cluster_operation::ClusterOperation,
        product_image_selection::ProductImage,
        resources::{
            CpuLimitsFragment, MemoryLimitsFragment, NoRuntimeLimits, NoRuntimeLimitsFragment,
            PvcConfig, PvcConfigFragment, Resources, ResourcesFragment,
        },
    },
    config::{
        fragment::Fragment,
        fragment::{self, ValidationError},
        merge::Merge,
    },
    k8s_openapi::apimachinery::pkg::api::resource::Quantity,
    kube::{runtime::reflector::ObjectRef, CustomResource, ResourceExt},
    product_config_utils::{ConfigError, Configuration},
    product_logging::{self, spec::Logging},
    role_utils::{Role, RoleGroup, RoleGroupRef},
    schemars::{self, JsonSchema},
};
use strum::Display;

pub const APP_NAME: &str = "nifi";

pub const HTTPS_PORT_NAME: &str = "https";
pub const HTTPS_PORT: u16 = 8443;
pub const PROTOCOL_PORT_NAME: &str = "protocol";
pub const PROTOCOL_PORT: u16 = 9088;
pub const BALANCE_PORT_NAME: &str = "balance";
pub const BALANCE_PORT: u16 = 6243;
pub const METRICS_PORT_NAME: &str = "metrics";
pub const METRICS_PORT: u16 = 8081;

pub const STACKABLE_LOG_DIR: &str = "/stackable/log";
pub const STACKABLE_LOG_CONFIG_DIR: &str = "/stackable/log_config";

pub const MAX_ZK_LOG_FILES_SIZE_IN_MIB: u32 = 10;
const MAX_PREPARE_LOG_FILE_SIZE_IN_MIB: u32 = 1;
// Additional buffer space is not needed, as the `prepare` container already has sufficient buffer
// space and all containers share a single volume.
pub const LOG_VOLUME_SIZE_IN_MIB: u32 =
    MAX_ZK_LOG_FILES_SIZE_IN_MIB + MAX_PREPARE_LOG_FILE_SIZE_IN_MIB;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object has no namespace associated"))]
    NoNamespace,
    #[snafu(display("the NiFi role [{role}] is missing from spec"))]
    MissingNifiRole { role: String },
    #[snafu(display("the NiFi node role group [{role_group}] is missing from spec"))]
    MissingNifiRoleGroup { role_group: String },
    #[snafu(display("fragment validation failure"))]
    FragmentValidationFailure { source: ValidationError },
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
    /// The NiFi image to use
    pub image: ProductImage,
    /// Global Nifi config for e.g. authentication or sensitive properties
    pub cluster_config: NifiClusterConfig,
    /// Cluster operations like pause reconciliation or cluster stop.
    #[serde(default)]
    pub cluster_operation: ClusterOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Available NiFi roles
    pub nodes: Option<Role<NifiConfigFragment>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiClusterConfig {
    /// A reference to a Secret containing username/password for the initial admin user
    pub authentication: NifiAuthenticationConfig,
    /// Configuration options for how NiFi encrypts sensitive properties on disk
    pub sensitive_properties: NifiSensitivePropertiesConfig,
    /// Name of the Vector aggregator discovery ConfigMap.
    /// It must contain the key `ADDRESS` with the address of the Vector aggregator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_aggregator_config_map_name: Option<String>,
    /// The reference to the ZooKeeper cluster
    pub zookeeper_config_map_name: String,
    /// Extra volumes to mount into every container, this can be useful to for example make client
    /// certificates, keytabs or similar things available to processors
    /// These volumes will be mounted below `/stackable/userdata/{volumename}`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_volumes: Vec<Volume>,
    /// In the future this setting will control, which ListenerClass <https://docs.stackable.tech/home/stable/listener-operator/listenerclass.html>
    /// will be used to expose the service.
    /// Currently only a subset of the ListenerClasses are supported by choosing the type of the created Services
    /// by looking at the ListenerClass name specified,
    /// In a future release support for custom ListenerClasses will be introduced without a breaking change:
    ///
    /// * cluster-internal: Use a ClusterIP service
    ///
    /// * external-unstable: Use a NodePort service
    ///
    /// * external-stable: Use a LoadBalancer service
    #[serde(default)]
    pub listener_class: CurrentlySupportedListenerClasses,
}

// TODO: Temporary solution until listener-operator is finished
#[derive(Clone, Debug, Default, Display, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum CurrentlySupportedListenerClasses {
    #[default]
    #[serde(rename = "cluster-internal")]
    ClusterInternal,
    #[serde(rename = "external-unstable")]
    ExternalUnstable,
    #[serde(rename = "external-stable")]
    ExternalStable,
}

impl CurrentlySupportedListenerClasses {
    pub fn k8s_service_type(&self) -> String {
        match self {
            CurrentlySupportedListenerClasses::ClusterInternal => "ClusterIP".to_string(),
            CurrentlySupportedListenerClasses::ExternalUnstable => "NodePort".to_string(),
            CurrentlySupportedListenerClasses::ExternalStable => "LoadBalancer".to_string(),
        }
    }
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

#[derive(
    Clone,
    Debug,
    Deserialize,
    strum::Display,
    Eq,
    strum::EnumIter,
    JsonSchema,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Container {
    Prepare,
    Vector,
    Nifi,
}

#[derive(Clone, Debug, Default, Fragment, JsonSchema, PartialEq)]
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
pub struct NifiConfig {
    #[fragment_attrs(serde(default))]
    pub logging: Logging<Container>,
    #[fragment_attrs(serde(default))]
    pub resources: Resources<NifiStorageConfig, NoRuntimeLimits>,
    #[fragment_attrs(serde(default))]
    pub affinity: StackableAffinity,
}

impl NifiConfig {
    pub const NIFI_SENSITIVE_PROPS_KEY: &'static str = "NIFI_SENSITIVE_PROPS_KEY";

    pub fn default_config(cluster_name: &str, role: &NifiRole) -> NifiConfigFragment {
        NifiConfigFragment {
            logging: product_logging::spec::default_logging(),
            resources: ResourcesFragment {
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
            },
            affinity: get_affinity(cluster_name, role),
        }
    }
}

impl Configuration for NifiConfigFragment {
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

    /// Retrieve and merge resource configs for role and role groups
    pub fn merged_config(&self, role: &NifiRole, role_group: &str) -> Result<NifiConfig, Error> {
        // Initialize the result with all default values as baseline
        let conf_defaults = NifiConfig::default_config(&self.name_any(), role);

        let role = self.spec.nodes.as_ref().context(MissingNifiRoleSnafu {
            role: role.to_string(),
        })?;

        // Retrieve role resource config
        let mut conf_role = role.config.config.to_owned();

        // Retrieve rolegroup specific resource config
        let mut conf_rolegroup = role
            .role_groups
            .get(role_group)
            .map(|rg| rg.config.config.clone())
            .unwrap_or_default();

        if let Some(RoleGroup {
            selector: Some(selector),
            ..
        }) = role.role_groups.get(role_group)
        {
            // Migrate old `selector` attribute, see ADR 26 affinities.
            // TODO Can be removed after support for the old `selector` field is dropped.
            #[allow(deprecated)]
            conf_rolegroup.affinity.add_legacy_selector(selector);
        }

        // Merge more specific configs into default config
        // Hierarchy is:
        // 1. RoleGroup
        // 2. Role
        // 3. Default
        conf_role.merge(&conf_defaults);
        conf_rolegroup.merge(&conf_role);

        tracing::debug!("Merged config: {:?}", conf_rolegroup);
        fragment::validate(conf_rolegroup).context(FragmentValidationFailureSnafu)
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
