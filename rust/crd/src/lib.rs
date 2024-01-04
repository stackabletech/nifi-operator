pub mod affinity;
pub mod authentication;

use crate::authentication::NifiAuthenticationClassRef;

use affinity::get_affinity;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
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
    k8s_openapi::{api::core::v1::Volume, apimachinery::pkg::api::resource::Quantity},
    kube::{runtime::reflector::ObjectRef, CustomResource, ResourceExt},
    memory::{BinaryMultiple, MemoryQuantity},
    product_config_utils::{ConfigError, Configuration},
    product_logging::{self, spec::Logging},
    role_utils::{GenericRoleConfig, Role, RoleGroupRef},
    schemars::{self, JsonSchema},
    status::condition::{ClusterCondition, HasStatusCondition},
    time::Duration,
};
use std::collections::BTreeMap;
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

pub const MAX_NIFI_LOG_FILES_SIZE: MemoryQuantity = MemoryQuantity {
    value: 10.0,
    unit: BinaryMultiple::Mebi,
};
pub const MAX_PREPARE_LOG_FILE_SIZE: MemoryQuantity = MemoryQuantity {
    value: 1.0,
    unit: BinaryMultiple::Mebi,
};

const DEFAULT_NODE_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_minutes_unchecked(5);

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

/// A NiFi cluster stacklet. This resource is managed by the Stackable operator for Apache NiFi.
/// Find more information on how to use it and the resources that the operator generates in the
/// [operator documentation](DOCS_BASE_URL_PLACEHOLDER/nifi/).
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
    /// Settings that affect all roles and role groups.
    /// The settings in the `clusterConfig` are cluster wide settings that do not need to be configurable at role or role group level.
    pub cluster_config: NifiClusterConfig,

    // no doc - docs in Role struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Role<NifiConfigFragment>>,

    // no doc - docs in ProductImage struct.
    pub image: ProductImage,

    // no doc - docs in ClusterOperation struct.
    #[serde(default)]
    pub cluster_operation: ClusterOperation,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiClusterConfig {
    /// Authentication options for NiFi (required).
    /// Read more about authentication in the [security documentation](DOCS_BASE_URL_PLACEHOLDER/nifi/usage_guide/security).
    // We don't add `#[serde(default)]` here, as we require authentication
    pub authentication: Vec<NifiAuthenticationClassRef>,

    // no doc - docs in NifiSensitivePropertiesConfig struct.
    pub sensitive_properties: NifiSensitivePropertiesConfig,

    /// Name of the Vector aggregator [discovery ConfigMap](DOCS_BASE_URL_PLACEHOLDER/concepts/service_discovery).
    /// It must contain the key `ADDRESS` with the address of the Vector aggregator.
    /// Follow the [logging tutorial](DOCS_BASE_URL_PLACEHOLDER/tutorials/logging-vector-aggregator)
    /// to learn how to configure log aggregation with Vector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_aggregator_config_map_name: Option<String>,

    /// NiFi requires a ZooKeeper cluster connection to run.
    /// Provide the name of the ZooKeeper [discovery ConfigMap](DOCS_BASE_URL_PLACEHOLDER/concepts/service_discovery)
    /// here. When using the [Stackable operator for Apache ZooKeeper](DOCS_BASE_URL_PLACEHOLDER/zookeeper/)
    /// to deploy a ZooKeeper cluster, this will simply be the name of your ZookeeperCluster resource.
    pub zookeeper_config_map_name: String,

    /// Extra volumes to mount into every container, this can be useful to for example make client
    /// certificates, keytabs or similar things available to processors
    /// These volumes will be mounted below `/stackable/userdata/{volumename}`.
    /// See also the [external files usage guide](DOCS_BASE_URL_PLACEHOLDER/nifi/usage_guide/extra-volumes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_volumes: Vec<Volume>,

    /// This field controls which type of Service the Operator creates for this NifiCluster:
    ///
    /// * cluster-internal: Use a ClusterIP service
    ///
    /// * external-unstable: Use a NodePort service
    ///
    /// This is a temporary solution with the goal to keep yaml manifests forward compatible.
    /// In the future, this setting will control which [ListenerClass](DOCS_BASE_URL_PLACEHOLDER/listener-operator/listenerclass.html)
    /// will be used to expose the service, and ListenerClass names will stay the same, allowing for a non-breaking change.
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
}

impl CurrentlySupportedListenerClasses {
    pub fn k8s_service_type(&self) -> String {
        match self {
            CurrentlySupportedListenerClasses::ClusterInternal => "ClusterIP".to_string(),
            CurrentlySupportedListenerClasses::ExternalUnstable => "NodePort".to_string(),
        }
    }
}

/// These settings configure the encryption of sensitive properties in NiFi processors.
/// NiFi supports encrypting sensitive properties in processors as they are written to disk.
/// You can configure the encryption algorithm and the key to use.
/// You can also let the operator generate an encryption key for you.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiSensitivePropertiesConfig {
    /// A reference to a Secret. The Secret needs to contain a key `nifiSensitivePropsKey`.
    /// If `autoGenerate` is false and this object is missing, the Operator will raise an error.
    /// The encryption key needs to be at least 12 characters long.
    pub key_secret: String,

    /// Whether to generate the `keySecret` if it is missing.
    /// Defaults to `false`.
    #[serde(default)]
    pub auto_generate: bool,

    /// This is setting the `nifi.sensitive.props.algorithm` property in NiFi.
    /// This setting configures the encryption algorithm to use to encrypt sensitive properties.
    /// Valid values are:
    ///
    /// `nifiPbkdf2AesGcm256` (the default value),
    /// `nifiArgon2AesGcm256`,
    ///
    /// The following algorithms are deprecated and will be removed in future versions:
    ///
    /// `nifiArgon2AesGcm128`,
    /// `nifiBcryptAesGcm128`,
    /// `nifiBcryptAesGcm256`,
    /// `nifiPbkdf2AesGcm128`,
    /// `nifiScryptAesGcm128`,
    /// `nifiScryptAesGcm256`.
    ///
    /// Learn more about the specifics of the algorithm parameters in the
    /// [NiFi documentation](https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#property-encryption-algorithms).
    pub algorithm: Option<NifiSensitiveKeyAlgorithm>,
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
    #[serde(default)]
    pub conditions: Vec<ClusterCondition>,
}

impl HasStatusCondition for NifiCluster {
    fn conditions(&self) -> Vec<ClusterCondition> {
        match &self.status {
            Some(status) => status.conditions.clone(),
            None => vec![],
        }
    }
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

    /// Resource usage is configured here, this includes CPU usage, memory usage and disk storage usage.
    /// The default CPU request and limit are 500m and 2000m respectively.
    /// The default memory limit is 4GB.
    #[fragment_attrs(serde(default))]
    pub resources: Resources<NifiStorageConfig, NoRuntimeLimits>,

    #[fragment_attrs(serde(default))]
    pub affinity: StackableAffinity,

    /// Time period Pods have to gracefully shut down, e.g. `30m`, `1h` or `2d`. Consult the operator documentation for details.
    #[fragment_attrs(serde(default))]
    pub graceful_shutdown_timeout: Option<Duration>,
}

impl NifiConfig {
    pub const NIFI_SENSITIVE_PROPS_KEY: &'static str = "NIFI_SENSITIVE_PROPS_KEY";

    pub fn default_config(cluster_name: &str, role: &NifiRole) -> NifiConfigFragment {
        NifiConfigFragment {
            logging: product_logging::spec::default_logging(),
            resources: ResourcesFragment {
                cpu: CpuLimitsFragment {
                    min: Some(Quantity("500m".to_string())),
                    max: Some(Quantity("2000m".to_string())),
                },
                memory: MemoryLimitsFragment {
                    limit: Some(Quantity("4096Mi".to_string())),
                    runtime_limits: NoRuntimeLimitsFragment {},
                },
                storage: NifiStorageConfigFragment {
                    flowfile_repo: PvcConfigFragment {
                        capacity: Some(Quantity("1024Mi".to_string())),
                        storage_class: None,
                        selectors: None,
                    },
                    provenance_repo: PvcConfigFragment {
                        capacity: Some(Quantity("2048Mi".to_string())),
                        storage_class: None,
                        selectors: None,
                    },
                    database_repo: PvcConfigFragment {
                        capacity: Some(Quantity("1024Mi".to_string())),
                        storage_class: None,
                        selectors: None,
                    },
                    content_repo: PvcConfigFragment {
                        capacity: Some(Quantity("4096Mi".to_string())),
                        storage_class: None,
                        selectors: None,
                    },
                    state_repo: PvcConfigFragment {
                        capacity: Some(Quantity("1024Mi".to_string())),
                        storage_class: None,
                        selectors: None,
                    },
                },
            },
            affinity: get_affinity(cluster_name, role),
            graceful_shutdown_timeout: Some(DEFAULT_NODE_GRACEFUL_SHUTDOWN_TIMEOUT),
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
    /// [The FlowFile Repository](https://nifi.apache.org/docs/nifi-docs/html/nifi-in-depth.html#flowfile-repository)
    /// is where NiFi keeps track of the state and metadata of FlowFiles as they traverse the data flow.
    /// The repository ensures durability, reliability, and recoverability of data in case of system failures or interruptions.
    ///
    /// Default size: 1GB
    #[fragment_attrs(serde(default))]
    pub flowfile_repo: PvcConfig,

    /// [The Provenance Repository](https://nifi.apache.org/docs/nifi-docs/html/nifi-in-depth.html#provenance-repository)
    /// is where the history of each FlowFile is stored.
    /// This history is used to provide the Data Lineage (also known as the Chain of Custody) of each piece of data.
    ///
    /// Default size: 2GB
    #[fragment_attrs(serde(default))]
    pub provenance_repo: PvcConfig,

    /// Default size: 1GB
    #[fragment_attrs(serde(default))]
    pub database_repo: PvcConfig,

    /// [The Content Repository](https://nifi.apache.org/docs/nifi-docs/html/nifi-in-depth.html#content-repository)
    /// is simply a place in local storage where the content of all FlowFiles exists and it is typically the largest of the Repositories.
    ///
    /// Default size: 4GB
    #[fragment_attrs(serde(default))]
    pub content_repo: PvcConfig,

    /// Default size: 1GB
    #[fragment_attrs(serde(default))]
    pub state_repo: PvcConfig,
}

impl NifiCluster {
    /// The name of the role-level load-balanced Kubernetes `Service`
    pub fn node_role_service_name(&self) -> String {
        self.name_any()
    }

    /// The fully-qualified domain name of the role-level load-balanced Kubernetes `Service`
    pub fn node_role_service_fqdn(&self) -> Option<String> {
        Some(format!(
            "{}.{}.svc.cluster.local",
            self.node_role_service_name(),
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

    pub fn role_config(&self, role: &NifiRole) -> Option<&GenericRoleConfig> {
        match role {
            NifiRole::Node => self.spec.nodes.as_ref().map(|n| &n.role_config),
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
