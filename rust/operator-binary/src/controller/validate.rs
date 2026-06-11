//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedCluster`], consumed by the rest of `reconcile_nifi`.

use std::collections::BTreeMap;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    kube::ResourceExt as _,
    role_utils::JavaCommonConfig,
    v2::controller_utils::{self, get_cluster_name, get_uid},
};
use strum::{EnumDiscriminants, IntoStaticStr};

use super::{ValidatedCluster, ValidatedClusterConfig};
use crate::{
    controller::dereference::DereferencedObjects,
    crd::{NifiConfig, NifiRole, sensitive_properties, v1alpha1},
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

    #[snafu(display("failed to build the config for a rolegroup"))]
    BuildRoleGroupConfig {
        source: crate::framework::role_utils::Error,
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
            crate::nifi_controller::CONTAINER_IMAGE_BASE_NAME,
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
                .context(BuildRoleGroupConfigSnafu)?;
        groups.insert(rg_name.clone(), validated_rg);
    }

    let mut role_group_configs = BTreeMap::new();
    role_group_configs.insert(NifiRole::Node, groups);
    Ok(role_group_configs)
}
