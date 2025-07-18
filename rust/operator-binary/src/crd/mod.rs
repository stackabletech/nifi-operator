pub mod affinity;
pub mod authentication;
pub mod sensitive_properties;
pub mod tls;

use std::collections::BTreeMap;

use affinity::get_affinity;
use sensitive_properties::NifiSensitivePropertiesConfig;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    commons::{
        affinity::StackableAffinity,
        cache::UserInformationCache,
        cluster_operation::ClusterOperation,
        opa::OpaConfig,
        product_image_selection::ProductImage,
        resources::{
            CpuLimitsFragment, MemoryLimitsFragment, NoRuntimeLimits, NoRuntimeLimitsFragment,
            PvcConfig, PvcConfigFragment, Resources, ResourcesFragment,
        },
    },
    config::{
        fragment::{self, Fragment, ValidationError},
        merge::Merge,
    },
    crd::{authentication::core as auth_core, git_sync},
    k8s_openapi::{
        api::core::v1::{PodTemplateSpec, Volume},
        apimachinery::pkg::api::resource::Quantity,
    },
    kube::{CustomResource, ResourceExt, runtime::reflector::ObjectRef},
    memory::MemoryQuantity,
    product_config_utils::{self, Configuration},
    product_logging::{self, spec::Logging},
    role_utils::{GenericRoleConfig, JavaCommonConfig, Role, RoleGroupRef},
    schemars::{self, JsonSchema},
    status::condition::{ClusterCondition, HasStatusCondition},
    time::Duration,
    utils::crds::{raw_object_list_schema, raw_object_schema},
    versioned::versioned,
};
use tls::NifiTls;

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

pub const MAX_NIFI_LOG_FILES_SIZE: MemoryQuantity = MemoryQuantity::from_mebi(10.0);

const DEFAULT_NODE_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_minutes_unchecked(5);

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("the NiFi role [{role}] is missing from spec"))]
    MissingNifiRole { role: String },

    #[snafu(display("fragment validation failure"))]
    FragmentValidationFailure { source: ValidationError },
}

#[versioned(
    version(name = "v1alpha1"),
    crates(
        kube_core = "stackable_operator::kube::core",
        kube_client = "stackable_operator::kube::client",
        k8s_openapi = "stackable_operator::k8s_openapi",
        schemars = "stackable_operator::schemars",
        versioned = "stackable_operator::versioned"
    )
)]
pub mod versioned {
    /// A NiFi cluster stacklet. This resource is managed by the Stackable operator for Apache NiFi.
    /// Find more information on how to use it and the resources that the operator generates in the
    /// [operator documentation](DOCS_BASE_URL_PLACEHOLDER/nifi/).
    #[versioned(crd(
        group = "nifi.stackable.tech",
        shortname = "nifi",
        status = "NifiStatus",
        namespaced,
    ))]
    #[derive(Clone, CustomResource, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct NifiClusterSpec {
        /// Settings that affect all roles and role groups.
        /// The settings in the `clusterConfig` are cluster wide settings that do not need to be configurable at role or role group level.
        pub cluster_config: v1alpha1::NifiClusterConfig,

        // no doc - docs in Role struct.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub nodes: Option<Role<NifiConfigFragment, NifiNodeRoleConfig, JavaCommonConfig>>,

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
        /// Read more about authentication in the [security documentation](DOCS_BASE_URL_PLACEHOLDER/nifi/usage_guide/security#authentication).
        // We don't add `#[serde(default)]` here, as we require authentication
        pub authentication: Vec<auth_core::v1alpha1::ClientAuthenticationDetails>,

        /// Authorization options.
        /// Learn more in the [NiFi authorization usage guide](DOCS_BASE_URL_PLACEHOLDER/nifi/usage-guide/security#authorization).
        #[serde(skip_serializing_if = "Option::is_none")]
        pub authorization: Option<NifiAuthorization>,

        /// Configuration of allowed proxies e.g. load balancers or Kubernetes Ingress. Using a proxy that is not allowed by NiFi results
        /// in a failed host header check.
        #[serde(default)]
        pub host_header_check: HostHeaderCheckConfig,

        /// TLS configuration options for the server.
        #[serde(default)]
        pub tls: NifiTls,

        // no doc - docs in NifiSensitivePropertiesConfig struct.
        pub sensitive_properties: NifiSensitivePropertiesConfig,

        /// Name of the Vector aggregator [discovery ConfigMap](DOCS_BASE_URL_PLACEHOLDER/concepts/service_discovery).
        /// It must contain the key `ADDRESS` with the address of the Vector aggregator.
        /// Follow the [logging tutorial](DOCS_BASE_URL_PLACEHOLDER/tutorials/logging-vector-aggregator)
        /// to learn how to configure log aggregation with Vector.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub vector_aggregator_config_map_name: Option<String>,

        #[serde(flatten)]
        pub clustering_backend: NifiClusteringBackend,

        /// The `customComponentsGitSync` setting allows configuring custom components to mount via `git-sync`.
        /// Learn more in the documentation for
        /// [Loading custom components](DOCS_BASE_URL_PLACEHOLDER/nifi/usage_guide/custom-components.html#git_sync).
        #[serde(default)]
        pub custom_components_git_sync: Vec<git_sync::v1alpha1::GitSync>,

        /// Extra volumes similar to `.spec.volumes` on a Pod to mount into every container, this can be useful to for
        /// example make client certificates, keytabs or similar things available to processors. These volumes will be
        /// mounted into all pods at `/stackable/userdata/{volumename}`.
        /// See also the [external files usage guide](DOCS_BASE_URL_PLACEHOLDER/nifi/usage_guide/extra-volumes).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[schemars(schema_with = "raw_object_list_schema")]
        pub extra_volumes: Vec<Volume>,

        // Docs are on the struct
        #[serde(default)]
        pub create_reporting_task_job: CreateReportingTaskJob,
    }

    // This is flattened in for backwards compatibility reasons, `zookeeper_config_map_name` already existed and used to be mandatory.
    // For v1alpha2, consider migrating this to a tagged enum for consistency.
    #[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
    #[serde(untagged)]
    pub enum NifiClusteringBackend {
        #[serde(rename_all = "camelCase")]
        ZooKeeper {
            /// NiFi can either use ZooKeeper or Kubernetes for managing its cluster state. To use ZooKeeper, provide the name of the
            /// ZooKeeper [discovery ConfigMap](DOCS_BASE_URL_PLACEHOLDER/concepts/service_discovery) here.
            /// When using the [Stackable operator for Apache ZooKeeper](DOCS_BASE_URL_PLACEHOLDER/zookeeper/)
            /// to deploy a ZooKeeper cluster, this will simply be the name of your ZookeeperCluster resource.
            ///
            /// The Kubernetes provider will be used if this field is unset. Kubernetes is only supported for NiFi 2.x and newer,
            /// NiFi 1.x requires ZooKeeper.
            zookeeper_config_map_name: String,
        },
        Kubernetes {},
    }
}

impl HasStatusCondition for v1alpha1::NifiCluster {
    fn conditions(&self) -> Vec<ClusterCondition> {
        match &self.status {
            Some(status) => status.conditions.clone(),
            None => vec![],
        }
    }
}

impl v1alpha1::NifiCluster {
    /// Metadata about a metastore rolegroup
    pub fn node_rolegroup_ref(&self, group_name: impl Into<String>) -> RoleGroupRef<Self> {
        RoleGroupRef {
            cluster: ObjectRef::from_obj(self),
            role: NifiRole::Node.to_string(),
            role_group: group_name.into(),
        }
    }

    pub fn role_config(&self, role: &NifiRole) -> Option<&NifiNodeRoleConfig> {
        match role {
            NifiRole::Node => self.spec.nodes.as_ref().map(|n| &n.role_config),
        }
    }

    /// Return user provided server TLS settings
    pub fn server_tls_secret_class(&self) -> &str {
        &self.spec.cluster_config.tls.server_secret_class
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

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiAuthorization {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opa: Option<NifiOpaConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiOpaConfig {
    #[serde(flatten)]
    pub opa: OpaConfig,
    #[serde(default)]
    pub cache: UserInformationCache,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostHeaderCheckConfig {
    /// Allow all proxy hosts by turning off host header validation.
    /// See <https://github.com/stackabletech/docker-images/pull/694>
    #[serde(default = "default_allow_all")]
    pub allow_all: bool,
    /// List of proxy hosts to add to the default allow list deployed by SDP containing Kubernetes Services utilized by NiFi.
    #[serde(default)]
    pub additional_allowed_hosts: Vec<String>,
}

impl Default for HostHeaderCheckConfig {
    fn default() -> Self {
        Self {
            allow_all: default_allow_all(),
            additional_allowed_hosts: Vec::default(),
        }
    }
}

pub fn default_allow_all() -> bool {
    true
}

#[derive(strum::Display, Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StoreType {
    #[strum(serialize = "JKS")]
    Jks,
    #[strum(serialize = "PKCS12")]
    Pkcs12,
}

impl Default for StoreType {
    fn default() -> Self {
        Self::Jks
    }
}

/// This section creates a `create-reporting-task` Kubernetes Job, which enables the export of
/// Prometheus metrics within NiFi.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReportingTaskJob {
    /// Wether the Kubernetes Job should be created, defaults to true. It can be helpful to disable
    /// the Job, e.g. when you configOverride an authentication mechanism, which the Job currently
    /// can't use to authenticate against NiFi.
    #[serde(default = "CreateReportingTaskJob::default_enabled")]
    pub enabled: bool,

    /// Here you can define a
    /// [PodTemplateSpec](https://kubernetes.io/docs/reference/generated/kubernetes-api/v1.27/#podtemplatespec-v1-core)
    /// to override any property that can be set on the Pod of the create-reporting-task Kubernetes Job.
    /// Read the
    /// [Pod overrides documentation](DOCS_BASE_URL_PLACEHOLDER/concepts/overrides#pod-overrides)
    /// for more information.
    #[serde(default)]
    #[schemars(schema_with = "raw_object_schema")]
    pub pod_overrides: PodTemplateSpec,
}

impl Default for CreateReportingTaskJob {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            pod_overrides: Default::default(),
        }
    }
}

impl CreateReportingTaskJob {
    const fn default_enabled() -> bool {
        true
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
    GitSync,
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

    /// Request secret (currently only autoTls certificates) lifetime from the secret operator, e.g. `7d`, or `30d`.
    /// Please note that this can be shortened by the `maxCertificateLifetime` setting on the SecretClass issuing the TLS certificate.
    #[fragment_attrs(serde(default))]
    pub requested_secret_lifetime: Option<Duration>,
}

impl NifiConfig {
    // Auto TLS certificate lifetime
    const DEFAULT_NODE_SECRET_LIFETIME: Duration = Duration::from_days_unchecked(1);

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
            requested_secret_lifetime: Some(Self::DEFAULT_NODE_SECRET_LIFETIME),
        }
    }
}

impl Configuration for NifiConfigFragment {
    type Configurable = v1alpha1::NifiCluster;

    fn compute_env(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, product_config_utils::Error> {
        Ok(BTreeMap::new())
    }

    fn compute_cli(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
    ) -> Result<BTreeMap<String, Option<String>>, product_config_utils::Error> {
        Ok(BTreeMap::new())
    }

    fn compute_files(
        &self,
        _resource: &Self::Configurable,
        _role_name: &str,
        _file: &str,
    ) -> Result<BTreeMap<String, Option<String>>, product_config_utils::Error> {
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiNodeRoleConfig {
    #[serde(flatten)]
    pub common: GenericRoleConfig,

    #[serde(default = "node_default_listener_class")]
    pub listener_class: String,
}

impl Default for NifiNodeRoleConfig {
    fn default() -> Self {
        NifiNodeRoleConfig {
            listener_class: node_default_listener_class(),
            common: Default::default(),
        }
    }
}

fn node_default_listener_class() -> String {
    "cluster-internal".to_string()
}
