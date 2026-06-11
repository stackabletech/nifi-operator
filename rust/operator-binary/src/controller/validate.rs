//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedCluster`], consumed by the rest of `reconcile_nifi`.

use std::{collections::BTreeMap, str::FromStr as _};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    config::fragment,
    kube::ResourceExt as _,
    role_utils::CommonConfiguration,
    v2::{
        builder::pod::container::{self, EnvVarName, EnvVarSet},
        controller_utils::{self, get_cluster_name, get_uid},
        role_utils::with_validated_config,
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use super::{ValidatedCluster, ValidatedClusterConfig, ValidatedRoleGroupConfig};
use crate::{
    controller::dereference::DereferencedObjects,
    crd::{NifiConfig, NifiRole, sensitive_properties, v1alpha1},
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

    #[snafu(display("failed to validate the rolegroup config fragment"))]
    ValidateRoleGroupConfig { source: fragment::ValidationError },

    #[snafu(display("environment variable name {name:?} is invalid"))]
    ParseEnvVarName {
        source: container::Error,
        name: String,
    },

    #[snafu(display("invalid sensitive properties algorithm"))]
    InvalidSensitivePropertiesAlgorithm { source: sensitive_properties::Error },
}

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

    let role_group_configs = build_role_group_configs(nifi)?;

    let name = get_cluster_name(nifi).context(GetClusterNameSnafu)?;
    let namespace = dereferenced_objects.namespace.clone();
    let uid = get_uid(nifi).context(GetUidSnafu)?;

    Ok(ValidatedCluster::new(
        name,
        namespace,
        uid,
        image,
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

pub(crate) fn build_role_group_configs(
    nifi: &v1alpha1::NifiCluster,
) -> Result<BTreeMap<NifiRole, BTreeMap<String, ValidatedRoleGroupConfig>>> {
    let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;
    let default_config = NifiConfig::default_config(&nifi.name_any(), &NifiRole::Node);

    let mut groups: BTreeMap<String, ValidatedRoleGroupConfig> = BTreeMap::new();
    for (rg_name, rg) in &role.role_groups {
        let validated = with_validated_config::<NifiConfig, _, _, _, _>(rg, role, &default_config)
            .context(ValidateRoleGroupConfigSnafu)?;

        let CommonConfiguration {
            config,
            config_overrides,
            env_overrides,
            cli_overrides: _,
            pod_overrides,
            product_specific_common_config,
        } = validated.config;

        // Convert the merged env-override HashMap into an EnvVarSet, validating each name
        // eagerly. Keys are unique (HashMap), so insertion order is irrelevant.
        let mut env_overrides_set = EnvVarSet::new();
        for (name, value) in env_overrides {
            env_overrides_set = env_overrides_set.with_value(
                &EnvVarName::from_str(&name)
                    .context(ParseEnvVarNameSnafu { name: name.clone() })?,
                value,
            );
        }

        groups.insert(
            rg_name.clone(),
            ValidatedRoleGroupConfig {
                replicas: validated.replicas.unwrap_or(1),
                config,
                config_overrides,
                env_overrides: env_overrides_set,
                pod_overrides,
                product_specific_common_config,
            },
        );
    }

    let mut role_group_configs = BTreeMap::new();
    role_group_configs.insert(NifiRole::Node, groups);
    Ok(role_group_configs)
}
