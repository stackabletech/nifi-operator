//! Controller-level vocabulary: the [`ValidatedCluster`] type produced by the
//! [`validate`] step and consumed by the [`build`] steps, plus the
//! `dereference` / `validate` / `build` sub-modules.

use std::{collections::BTreeMap, str::FromStr as _};

use stackable_operator::{
    commons::product_image_selection::ResolvedProductImage,
    crd::git_sync,
    k8s_openapi::{api::core::v1::PodTemplateSpec, apimachinery::pkg::apis::meta::v1::ObjectMeta},
    kube::Resource,
    kvp::Labels,
    v2::{
        HasName, HasUid, NameIsValidLabelValue,
        builder::pod::container::EnvVarSet,
        kvp::label::{recommended_labels, role_group_selector, role_selector},
        product_logging::framework::VectorContainerLogConfig,
        role_group_utils::ResourceNames,
        role_utils::JavaCommonConfig,
        types::{
            kubernetes::{NamespaceName, Uid},
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
        APP_NAME, HostHeaderCheckConfig, NifiConfig, NifiRole,
        sensitive_properties::NifiSensitiveKeyAlgorithm, v1alpha1,
    },
    nifi_controller::NIFI_CONTROLLER_NAME,
    security::{
        authentication::NifiAuthenticationConfig, authorization::ResolvedNifiAuthorizationConfig,
    },
};

pub(crate) mod build;
pub(crate) mod dereference;
pub(crate) mod upgrade;
pub(crate) mod validate;

/// A validated, merged (default <- role <- role-group) NiFi rolegroup config.
///
/// Produced from the result of
/// [`with_validated_config`](stackable_operator::v2::role_utils::with_validated_config) in the
/// [`validate`] step; downstream builders consume this rather than the raw `NifiCluster`.
#[derive(Clone, Debug)]
pub struct ValidatedRoleGroupConfig {
    /// The desired number of replicas (defaulted to 1 during validation).
    ///
    /// The StatefulSet replica count is currently sourced from the raw role-group spec in
    /// `nifi_controller` (to keep `replicas: null` semantics during version updates), so this
    /// validated value is carried for completeness but not yet read.
    #[allow(dead_code)]
    pub replicas: u16,
    /// The merged and validated rolegroup config.
    pub config: NifiConfig,
    /// The merged (role <- role group) config-file overrides.
    pub config_overrides: v1alpha1::NifiConfigOverrides,
    /// The merged (role <- role group) environment variable overrides.
    pub env_overrides: EnvVarSet,
    /// The merged (role <- role group) pod template overrides.
    pub pod_overrides: PodTemplateSpec,
    /// The merged (role <- role group) JVM argument overrides, applied on top of the
    /// operator-generated JVM arguments when building `bootstrap.conf`.
    pub product_specific_common_config: JavaCommonConfig,
    /// The validated Vector container logging config (log config choice + aggregator discovery
    /// ConfigMap name), validated up-front in the [`validate`] step. `None` when the Vector agent
    /// is disabled for this role group.
    pub vector_container: Option<VectorContainerLogConfig>,
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
    /// The UID of the NifiCluster, used to build OwnerReferences downstream.
    pub uid: Uid,
    /// The product image.
    pub image: ResolvedProductImage,
    /// The product version as a type-safe label value, used for the `app.kubernetes.io/version`
    /// label on built resources.
    pub product_version: ProductVersion,
    /// Cluster wide settings.
    pub cluster_config: ValidatedClusterConfig,
    /// Collected configuration per rolegroup.
    pub role_group_configs: BTreeMap<NifiRole, BTreeMap<RoleGroupName, ValidatedRoleGroupConfig>>,
}

/// The resolved `spec.clusterConfig`.
pub struct ValidatedClusterConfig {
    /// The cluster authentication settings.
    pub authentication: NifiAuthenticationConfig,
    /// The cluster authorization settings.
    pub authorization: ResolvedNifiAuthorizationConfig,
    /// The git-sync specs, resolved into git-sync resources at build time.
    pub custom_components_git_sync: Vec<git_sync::v1alpha2::GitSync>,
    /// The clustering backend (ZooKeeper or Kubernetes), copied from the spec.
    pub clustering_backend: v1alpha1::NifiClusteringBackend,
    /// The host-header-check config, resolved into the proxy hosts allow-list at build time.
    pub host_header_check: HostHeaderCheckConfig,
    /// The validated sensitive properties algorithm.
    pub sensitive_properties_algorithm: NifiSensitiveKeyAlgorithm,
}

impl ValidatedCluster {
    /// Builds a [`ValidatedCluster`], deriving the synthetic [`ObjectMeta`] from name, namespace
    /// and uid so the struct can implement [`Resource`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: ClusterName,
        namespace: NamespaceName,
        uid: Uid,
        image: ResolvedProductImage,
        product_version: ProductVersion,
        role_group_configs: BTreeMap<NifiRole, BTreeMap<RoleGroupName, ValidatedRoleGroupConfig>>,
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
            uid,
            image,
            product_version,
            role_group_configs,
            cluster_config,
        }
    }

    /// The single NiFi role name (`node`).
    pub fn role_name() -> RoleName {
        RoleName::from_str(&NifiRole::Node.to_string())
            .expect("the node role name is a valid role name")
    }

    /// Type-safe names for the resources of a given role group.
    pub(crate) fn resource_names(&self, role_group_name: &RoleGroupName) -> ResourceNames {
        ResourceNames {
            cluster_name: self.name.clone(),
            role_name: Self::role_name(),
            role_group_name: role_group_name.clone(),
        }
    }

    /// Recommended labels for a role-group resource, using the given product version.
    fn recommended_labels_for(
        &self,
        product_version: &ProductVersion,
        role_group_name: &RoleGroupName,
    ) -> Labels {
        recommended_labels(
            self,
            &product_name(),
            product_version,
            &operator_name(),
            &controller_name(),
            &Self::role_name(),
            role_group_name,
        )
    }

    /// Recommended labels for a role-group resource.
    pub fn recommended_labels(&self, role_group_name: &RoleGroupName) -> Labels {
        self.recommended_labels_for(&self.product_version, role_group_name)
    }

    /// Recommended labels for resources whose labels must stay stable across version upgrades
    /// (e.g. PVC templates, which are immutable once created), using the placeholder version
    /// `none` for `app.kubernetes.io/version`.
    pub fn recommended_labels_unversioned(&self, role_group_name: &RoleGroupName) -> Labels {
        let unversioned = ProductVersion::from_str("none")
            .expect("'none' is a valid product version label value");
        self.recommended_labels_for(&unversioned, role_group_name)
    }

    /// Selector labels matching the pods of a role group.
    pub fn role_group_selector(&self, role_group_name: &RoleGroupName) -> Labels {
        role_group_selector(self, &product_name(), &Self::role_name(), role_group_name)
    }

    /// Selector labels matching all pods of the (single) NiFi role.
    pub fn role_selector(&self) -> Labels {
        role_selector(self, &product_name(), &Self::role_name())
    }

    /// Recommended labels for a role-level resource (the per-role [`Listener`]), which has no
    /// associated role group. Uses the placeholder role-group `none`, preserving the historical
    /// `app.kubernetes.io/role-group: none` label.
    pub fn recommended_labels_role_level(&self) -> Labels {
        let role_group =
            RoleGroupName::from_str("none").expect("'none' is a valid role-group name");
        self.recommended_labels(&role_group)
    }
}

/// The product name (`nifi`) as a type-safe label value.
fn product_name() -> ProductName {
    ProductName::from_str(APP_NAME).expect("'nifi' is a valid product name")
}

/// The operator name as a type-safe label value.
fn operator_name() -> OperatorName {
    OperatorName::from_str(OPERATOR_NAME).expect("the operator name is a valid label value")
}

/// The controller name as a type-safe label value.
fn controller_name() -> ControllerName {
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
