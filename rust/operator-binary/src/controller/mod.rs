//! Controller-level vocabulary: the [`ValidatedCluster`] type produced by the
//! [`validate`] step and consumed by the [`build`] steps, plus the
//! `dereference` / `validate` / `build` sub-modules.

use std::collections::BTreeMap;

use stackable_operator::{
    commons::product_image_selection::ResolvedProductImage,
    crd::git_sync,
    k8s_openapi::{api::core::v1::PodTemplateSpec, apimachinery::pkg::apis::meta::v1::ObjectMeta},
    kube::Resource,
    v2::{
        HasName, HasUid,
        builder::pod::container::EnvVarSet,
        role_utils::JavaCommonConfig,
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::ClusterName,
        },
    },
};

use crate::{
    crd::{
        HostHeaderCheckConfig, NifiConfig, NifiRole,
        sensitive_properties::NifiSensitiveKeyAlgorithm, v1alpha1,
    },
    security::{
        authentication::NifiAuthenticationConfig, authorization::ResolvedNifiAuthorizationConfig,
    },
};

pub(crate) mod build;
pub(crate) mod dereference;
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
    /// Cluster wide settings.
    pub cluster_config: ValidatedClusterConfig,
    /// Collected configuration per rolegroup.
    pub role_group_configs: BTreeMap<NifiRole, BTreeMap<String, ValidatedRoleGroupConfig>>,
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
    pub fn new(
        name: ClusterName,
        namespace: NamespaceName,
        uid: Uid,
        image: ResolvedProductImage,
        role_group_configs: BTreeMap<NifiRole, BTreeMap<String, ValidatedRoleGroupConfig>>,
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
            role_group_configs,
            cluster_config,
        }
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
