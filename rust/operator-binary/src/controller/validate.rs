//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedCluster`], consumed by the rest of `reconcile_nifi`.

use std::collections::BTreeMap;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection::{self, ResolvedProductImage},
    crd::git_sync,
    k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    kube::{Resource, ResourceExt as _},
    role_utils::JavaCommonConfig,
    v2::{
        HasName, HasUid,
        controller_utils::{self, get_cluster_name, get_uid},
        types::{
            kubernetes::{NamespaceName, Uid},
            operator::ClusterName,
        },
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    controller::dereference::DereferencedObjects,
    crd::{
        HostHeaderCheckConfig, NifiConfig, NifiRole, NifiRoleType, sensitive_properties,
        sensitive_properties::NifiSensitiveKeyAlgorithm, v1alpha1,
    },
    framework::role_utils::with_validated_config,
    security::{
        authentication::{self, NifiAuthenticationConfig},
        authorization::ResolvedNifiAuthorizationConfig,
    },
};

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("failed to resolve product image"))]
    ResolveProductImage {
        source: product_image_selection::Error,
    },

    #[snafu(display("object has no nodes defined"))]
    NoNodesDefined,

    #[snafu(display("failed to get the cluster name"))]
    GetClusterName { source: controller_utils::Error },

    #[snafu(display("failed to get the UID"))]
    GetUid { source: controller_utils::Error },

    #[snafu(display("invalid NiFi authentication configuration"))]
    InvalidAuthenticationConfig { source: authentication::Error },

    #[snafu(display("failed to validate config fragment for a rolegroup"))]
    InvalidConfigFragment {
        source: stackable_operator::config::fragment::ValidationError,
    },

    #[snafu(display("invalid sensitive properties algorithm"))]
    InvalidSensitivePropertiesAlgorithm { source: sensitive_properties::Error },
}

pub type NifiRoleGroupConfig = crate::framework::role_utils::RoleGroupConfig<
    NifiConfig,
    JavaCommonConfig,
    v1alpha1::NifiConfigOverrides,
>;

type Result<T, E = Error> = std::result::Result<T, E>;

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

/// Validates the cluster spec and the dereferenced inputs.
pub fn validate(
    nifi: &v1alpha1::NifiCluster,
    dereferenced_objects: &DereferencedObjects,
    operator_environment: &OperatorEnvironmentOptions,
) -> Result<ValidatedCluster> {
    let image = nifi
        .spec
        .image
        .resolve(
            super::CONTAINER_IMAGE_BASE_NAME,
            &operator_environment.image_repository,
            crate::built_info::PKG_VERSION,
        )
        .context(ResolveProductImageSnafu)?;

    let authentication_config =
        NifiAuthenticationConfig::validate(nifi, &dereferenced_objects.authentication_classes)
            .context(InvalidAuthenticationConfigSnafu)?;

    let authorization_config = ResolvedNifiAuthorizationConfig::validate(
        &nifi.spec.cluster_config.authorization,
        &dereferenced_objects.authorization,
    );

    let sensitive_properties_algorithm = nifi
        .spec
        .cluster_config
        .sensitive_properties
        .algorithm
        .clone()
        .unwrap_or_default();
    sensitive_properties_algorithm
        .check_for_nifi_version(&image.product_version)
        .context(InvalidSensitivePropertiesAlgorithmSnafu)?;

    let nodes = nifi
        .spec
        .nodes
        .as_ref()
        .context(NoNodesDefinedSnafu)?
        .clone();
    let role_group_configs = build_role_group_configs(nifi)?;

    let name = get_cluster_name(nifi).context(GetClusterNameSnafu)?;
    let namespace = dereferenced_objects.namespace.clone();
    let uid = get_uid(nifi).context(GetUidSnafu)?;

    Ok(ValidatedCluster::new(
        name,
        namespace,
        uid,
        image,
        nodes,
        role_group_configs,
        ValidatedClusterConfig {
            authentication: authentication_config,
            authorization: authorization_config,
            clustering_backend: nifi.spec.cluster_config.clustering_backend.clone(),
            sensitive_properties_algorithm,
            host_header_check: nifi.spec.cluster_config.host_header_check.clone(),
            custom_components_git_sync: nifi.spec.cluster_config.custom_components_git_sync.clone(),
        },
    ))
}

fn build_role_group_configs(
    nifi: &v1alpha1::NifiCluster,
) -> Result<BTreeMap<NifiRole, BTreeMap<String, NifiRoleGroupConfig>>> {
    let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;
    let default_config = NifiConfig::default_config(&nifi.name_any(), &NifiRole::Node);

    let mut groups: BTreeMap<String, NifiRoleGroupConfig> = BTreeMap::new();
    for (rg_name, rg) in &role.role_groups {
        let validated_rg =
            with_validated_config::<NifiConfig, _, _, _, _>(rg, role, &default_config)
                .context(InvalidConfigFragmentSnafu)?;
        groups.insert(rg_name.clone(), validated_rg);
    }

    let mut role_group_configs = BTreeMap::new();
    role_group_configs.insert(NifiRole::Node, groups);
    Ok(role_group_configs)
}
