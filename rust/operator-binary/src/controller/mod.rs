//! Controller-level vocabulary: the [`ValidatedCluster`] type produced by the
//! [`validate`] step and consumed by the [`build`] steps, plus the
//! `dereference` / `validate` / `build` sub-modules.

use std::{collections::BTreeMap, str::FromStr as _};

use stackable_operator::{
    commons::{
        affinity::StackableAffinity,
        networking::DomainName,
        product_image_selection::ResolvedProductImage,
        resources::{NoRuntimeLimits, Resources},
    },
    crd::{git_sync, listener},
    k8s_openapi::{
        api::{
            apps::v1::StatefulSet,
            core::v1::{ConfigMap, Service, ServiceAccount, Volume},
            policy::v1::PodDisruptionBudget,
            rbac::v1::RoleBinding,
        },
        apimachinery::pkg::apis::meta::v1::ObjectMeta,
    },
    kube::Resource,
    kvp::Labels,
    shared::time::Duration,
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        kvp::label::{recommended_labels, role_group_selector},
        product_logging::framework::{ValidatedContainerLogConfigChoice, VectorContainerLogConfig},
        role_group_utils::ResourceNames,
        role_utils::{self, JavaCommonConfig, RoleGroupConfig},
        types::{
            kubernetes::{ListenerClassName, NamespaceName, SecretClassName, SecretName, Uid},
            operator::{
                ClusterName, ControllerName, OperatorName, ProductName, ProductVersion,
                RoleGroupName, RoleName,
            },
        },
    },
};

use crate::{
    OPERATOR_NAME,
    crd::{
        APP_NAME, HostHeaderCheckConfig, NifiConfig, NifiRole, NifiStorageConfig,
        sensitive_properties::NifiSensitiveKeyAlgorithm, v1alpha1,
    },
    nifi_controller::NIFI_CONTROLLER_NAME,
    security::{
        authentication::NifiAuthenticationConfig, authorization::ResolvedNifiAuthorizationConfig,
    },
};

pub(crate) mod build;
pub(crate) mod dereference;
pub(crate) mod validate;

// Placeholder version label value for resources whose labels must not change after deployment.
stackable_operator::constant!(UNVERSIONED_PRODUCT_VERSION: ProductVersion = "none");

/// Every Kubernetes resource produced by the [`build`] step.
pub struct KubernetesResources {
    pub stateful_sets: Vec<StatefulSet>,
    pub services: Vec<Service>,
    pub listeners: Vec<listener::v1alpha1::Listener>,
    pub config_maps: Vec<ConfigMap>,
    pub pod_disruption_budgets: Vec<PodDisruptionBudget>,
    pub service_accounts: Vec<ServiceAccount>,
    pub role_bindings: Vec<RoleBinding>,
}

/// A validated, merged (default <- role <- role-group) NiFi rolegroup config.
pub type NifiRoleGroupConfig =
    RoleGroupConfig<ValidatedNifiConfig, JavaCommonConfig, v1alpha1::NifiConfigOverrides>;

/// A validated NiFi [`NifiConfig`].
pub struct ValidatedNifiConfig {
    /// Resource requests/limits (CPU, memory and disk storage).
    pub resources: Resources<NifiStorageConfig, NoRuntimeLimits>,
    /// Pod (anti-)affinity for the role group.
    pub affinity: StackableAffinity,
    /// Time period Pods have to gracefully shut down.
    pub graceful_shutdown_timeout: Option<Duration>,
    /// Requested lifetime of the auto-TLS secret.
    pub requested_secret_lifetime: Option<Duration>,
    /// The validated logging configuration (NiFi and optional Vector container), validated up-front
    /// in the [`validate`] step.
    pub logging: ValidatedLogging,
    /// The git-sync resources (containers, volumes, mounts) for this role group, resolved from the
    /// cluster's `customComponentsGitSync` specs up-front in the [`validate`] step. The env vars and
    /// logging config differ per role group, so these are computed per role group. Consumed by both
    /// the StatefulSet builder and the `nifi.properties` builder.
    pub git_sync_resources: git_sync::v1alpha2::GitSyncResources,
}

impl ValidatedNifiConfig {
    pub(crate) fn from_merged(
        merged: NifiConfig,
        logging: ValidatedLogging,
        git_sync_resources: git_sync::v1alpha2::GitSyncResources,
    ) -> Self {
        Self {
            resources: merged.resources,
            affinity: merged.affinity,
            graceful_shutdown_timeout: merged.graceful_shutdown_timeout,
            requested_secret_lifetime: merged.requested_secret_lifetime,
            logging,
            git_sync_resources,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_merged_for_test(merged: NifiConfig) -> Self {
        use stackable_operator::product_logging::spec::AutomaticContainerLogConfig;

        Self::from_merged(
            merged,
            ValidatedLogging {
                nifi_container: ValidatedContainerLogConfigChoice::Automatic(
                    AutomaticContainerLogConfig::default(),
                ),
                prepare_container: ValidatedContainerLogConfigChoice::Automatic(
                    AutomaticContainerLogConfig::default(),
                ),
                vector_container: None,
                enable_vector_agent: false,
            },
            git_sync::v1alpha2::GitSyncResources::default(),
        )
    }
}

/// Validated logging configuration for the NiFi and (optional) Vector container.
///
/// Produced up-front by the [`validate`] step so that an invalid custom
/// log ConfigMap name, or a missing Vector aggregator discovery ConfigMap name, fails reconciliation
/// during validation rather than at resource-build time.
#[derive(Clone, Debug)]
pub struct ValidatedLogging {
    /// The NiFi container log config choice (automatic logging vs a custom log ConfigMap). Consumed
    /// by the `logback.xml` builder and the StatefulSet's `log-config` volume.
    pub nifi_container: ValidatedContainerLogConfigChoice,
    /// The `prepare` init-container log config choice. Consumed by the StatefulSet builder to
    /// capture the init container's shell output into the log directory (only for the `Automatic`
    /// choice).
    pub prepare_container: ValidatedContainerLogConfigChoice,
    /// The Vector container log config (log config choice + aggregator discovery ConfigMap name).
    /// `None` when the Vector agent is disabled for this role group.
    pub vector_container: Option<VectorContainerLogConfig>,
    /// Whether the Vector log agent is enabled for this role group.
    pub enable_vector_agent: bool,
}

/// The validated NifiCluster: everything `reconcile_nifi` needs after dereferencing,
/// in fail-safe / resolved form. This is the single resolved representation of the cluster;
/// downstream builders should source everything from here and never touch the raw `NifiCluster`.
pub struct ValidatedCluster {
    /// Synthetic metadata (name, namespace, uid) so `ValidatedCluster` can implement
    /// [`Resource`] and be used to build OwnerReferences without the raw `NifiCluster`.
    metadata: ObjectMeta,
    /// The name of the NifiCluster.
    pub name: ClusterName,
    /// The namespace of the NifiCluster, parsed once in the dereference step and reused everywhere.
    pub namespace: NamespaceName,
    /// The Kubernetes cluster domain, captured from the client in the dereference step so the
    /// build step needs no client to assemble in-cluster DNS names.
    pub cluster_domain: DomainName,
    /// The UID of the NifiCluster, used to build OwnerReferences downstream.
    pub uid: Uid,
    /// The product image.
    pub image: ResolvedProductImage,
    /// The product version as a type-safe label value, used for the `app.kubernetes.io/version`
    /// label on built resources.
    pub product_version: ProductVersion,
    /// Per-role configuration (PodDisruptionBudget and listener class). The `nodes` role is
    /// mandatory, so this is always present.
    pub role_config: ValidatedRoleConfig,
    /// Cluster wide settings.
    pub cluster_config: ValidatedClusterConfig,
    /// Collected configuration per rolegroup.
    pub role_group_configs: BTreeMap<NifiRole, BTreeMap<RoleGroupName, NifiRoleGroupConfig>>,
}

/// The resolved `spec.clusterConfig`.
pub struct ValidatedClusterConfig {
    /// The cluster authentication settings.
    pub authentication: NifiAuthenticationConfig,
    /// The cluster authorization settings.
    pub authorization: ResolvedNifiAuthorizationConfig,
    /// The clustering backend (ZooKeeper or Kubernetes), copied from the spec.
    pub clustering_backend: v1alpha1::NifiClusteringBackend,
    /// The host-header-check config, resolved into the proxy hosts allow-list at build time.
    pub host_header_check: HostHeaderCheckConfig,
    /// The resolved sensitive-properties configuration.
    pub sensitive_properties: ValidatedSensitiveProperties,
    /// The SecretClass providing the server TLS certificates.
    pub server_tls_secret_class: SecretClassName,
    /// User-provided extra volumes, mounted into every container under `/stackable/userdata/`.
    pub extra_volumes: Vec<Volume>,
}

/// The resolved `spec.clusterConfig.sensitiveProperties`.
pub struct ValidatedSensitiveProperties {
    /// The validated sensitive-properties encryption algorithm.
    pub algorithm: NifiSensitiveKeyAlgorithm,
    /// The name of the Secret holding the sensitive-properties key, mounted into the NiFi Pods.
    pub key_secret: SecretName,
    /// Whether to generate the key Secret if it is missing.
    pub auto_generate: bool,
}

/// Per-role configuration extracted during validation.
#[derive(Clone, Debug)]
pub struct ValidatedRoleConfig {
    pub pdb: stackable_operator::commons::pdb::PdbConfig,
    pub listener_class: ListenerClassName,
}

impl ValidatedCluster {
    /// Builds a [`ValidatedCluster`], deriving the synthetic [`ObjectMeta`] from name, namespace
    /// and uid so the struct can implement [`Resource`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: ClusterName,
        namespace: NamespaceName,
        cluster_domain: DomainName,
        uid: Uid,
        image: ResolvedProductImage,
        product_version: ProductVersion,
        role_config: ValidatedRoleConfig,
        role_group_configs: BTreeMap<NifiRole, BTreeMap<RoleGroupName, NifiRoleGroupConfig>>,
        cluster_config: ValidatedClusterConfig,
    ) -> Self {
        let metadata = ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            uid: Some(uid.to_string()),
            ..ObjectMeta::default()
        };

        Self {
            metadata,
            name,
            namespace,
            cluster_domain,
            uid,
            image,
            product_version,
            role_config,
            role_group_configs,
            cluster_config,
        }
    }

    /// The single NiFi role name (`node`).
    pub fn role_name() -> RoleName {
        RoleName::from_str(&NifiRole::Node.to_string())
            .expect("the node role name is a valid role name")
    }

    /// Type-safe names for the per-cluster RBAC resources: the ServiceAccount shared by all
    /// Pods, its (namespaced) RoleBinding, and the operator-deployed ClusterRole it binds.
    pub fn rbac_resource_names(&self) -> role_utils::ResourceNames {
        role_utils::ResourceNames {
            cluster_name: self.name.clone(),
            product_name: product_name(),
        }
    }

    /// Type-safe names for the resources of a given role group.
    pub(crate) fn resource_names(&self, role_group_name: &RoleGroupName) -> ResourceNames {
        ResourceNames {
            cluster_name: self.name.clone(),
            role_name: Self::role_name(),
            role_group_name: role_group_name.clone(),
        }
    }

    /// Recommended labels for a role-group resource.
    pub fn recommended_labels(&self, role_group_name: &RoleGroupName) -> Labels {
        self.recommended_labels_for(&Self::role_name(), role_group_name)
    }

    /// Recommended labels for a resource that is not tied to a concrete role, using a free-form role/role-group label value.
    pub fn recommended_labels_for(
        &self,
        role_name: &RoleName,
        role_group_name: &RoleGroupName,
    ) -> Labels {
        self.recommended_labels_with(&self.product_version, role_name, role_group_name)
    }

    /// Recommended labels with the constant [`UNVERSIONED_PRODUCT_VERSION`], for PVC templates
    /// that cannot be modified after deployment (keeps the labels stable across version upgrades).
    pub fn unversioned_recommended_labels(&self, role_group_name: &RoleGroupName) -> Labels {
        self.recommended_labels_with(
            &UNVERSIONED_PRODUCT_VERSION,
            &Self::role_name(),
            role_group_name,
        )
    }

    fn recommended_labels_with(
        &self,
        product_version: &ProductVersion,
        role_name: &RoleName,
        role_group_name: &RoleGroupName,
    ) -> Labels {
        recommended_labels(
            self,
            &product_name(),
            product_version,
            &operator_name(),
            &controller_name(),
            role_name,
            role_group_name,
        )
    }

    /// Selector labels matching the pods of a role group.
    pub fn role_group_selector(&self, role_group_name: &RoleGroupName) -> Labels {
        role_group_selector(self, &product_name(), &Self::role_name(), role_group_name)
    }

    /// Returns an [`ObjectMetaBuilder`](stackable_operator::builder::meta::ObjectMetaBuilder)
    /// pre-filled with the namespace, an owner reference back to this cluster, and the recommended
    /// labels for a resource named `name` in `role_group_name`.
    ///
    /// Consolidates the metadata chain repeated by the child-resource builders. Call sites that
    /// need extra labels/annotations chain them onto the returned builder. Role-level resources
    /// (e.g. the per-role [`Listener`](stackable_operator::crd::listener::v1alpha1::Listener)) pass
    /// the placeholder role-group `none`, preserving the historical
    /// `app.kubernetes.io/role-group: none` label.
    pub(crate) fn object_meta(
        &self,
        name: impl Into<String>,
        role_group_name: &RoleGroupName,
    ) -> stackable_operator::builder::meta::ObjectMetaBuilder {
        let mut builder = stackable_operator::builder::meta::ObjectMetaBuilder::new();
        builder
            .name_and_namespace(self)
            .name(name)
            .ownerreference(
                stackable_operator::v2::builder::meta::ownerreference_from_resource(
                    self,
                    None,
                    Some(true),
                ),
            )
            .with_labels(self.recommended_labels(role_group_name));
        builder
    }
}

/// The product name (`nifi`) as a type-safe label value.
pub(crate) fn product_name() -> ProductName {
    ProductName::from_str(APP_NAME).expect("'nifi' is a valid product name")
}

/// The operator name as a type-safe label value.
pub(crate) fn operator_name() -> OperatorName {
    OperatorName::from_str(OPERATOR_NAME).expect("the operator name is a valid label value")
}

/// The controller name as a type-safe label value.
pub(crate) fn controller_name() -> ControllerName {
    ControllerName::from_str(NIFI_CONTROLLER_NAME)
        .expect("the controller name is a valid label value")
}

impl NameIsValidLabelValue for ValidatedCluster {
    fn to_label_value(&self) -> String {
        self.name.to_label_value()
    }
}

impl HasName for ValidatedCluster {
    fn to_name(&self) -> String {
        self.name.to_string()
    }
}

impl HasUid for ValidatedCluster {
    fn to_uid(&self) -> Uid {
        self.uid.clone()
    }
}

impl Resource for ValidatedCluster {
    type DynamicType = <v1alpha1::NifiCluster as Resource>::DynamicType;
    type Scope = <v1alpha1::NifiCluster as Resource>::Scope;

    fn kind(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha1::NifiCluster::kind(dt)
    }

    fn group(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha1::NifiCluster::group(dt)
    }

    fn version(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha1::NifiCluster::version(dt)
    }

    fn plural(dt: &Self::DynamicType) -> std::borrow::Cow<'_, str> {
        v1alpha1::NifiCluster::plural(dt)
    }

    fn meta(&self) -> &ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut ObjectMeta {
        &mut self.metadata
    }
}
