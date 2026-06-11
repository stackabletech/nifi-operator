//! Controller-level vocabulary: the [`ValidatedCluster`] type produced by the
//! [`validate`] step and consumed by the [`build`] steps, plus the
//! `dereference` / `validate` / `build` sub-modules.

use std::collections::BTreeMap;

use stackable_operator::{
    commons::product_image_selection::ResolvedProductImage,
    crd::git_sync,
    k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    kube::Resource,
    v2::{
        HasName, HasUid,
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::ClusterName,
        },
    },
};

use crate::{
    crd::{
        HostHeaderCheckConfig, NifiRole, NifiRoleType,
        sensitive_properties::NifiSensitiveKeyAlgorithm, v1alpha1,
    },
    security::{
        authentication::NifiAuthenticationConfig, authorization::ResolvedNifiAuthorizationConfig,
    },
};

pub(crate) mod build;
pub(crate) mod dereference;
pub(crate) mod validate;

use validate::NifiRoleGroupConfig;

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
    /// The raw Node role spec (`spec.nodes`), needed for JVM argument merging in `bootstrap.conf`.
    pub nodes: NifiRoleType,
    /// Collected configuration per rolegroup.
    pub role_group_configs: BTreeMap<NifiRole, BTreeMap<String, NifiRoleGroupConfig>>,
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
        nodes: NifiRoleType,
        role_group_configs: BTreeMap<NifiRole, BTreeMap<String, NifiRoleGroupConfig>>,
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
            nodes,
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
