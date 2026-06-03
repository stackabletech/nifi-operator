//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedCluster`], consumed by the rest of `reconcile_nifi`.

use std::collections::{BTreeMap, HashSet};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection::{self, ResolvedProductImage},
    kube::ResourceExt as _,
    role_utils::JavaCommonConfig,
    utils::cluster_info::KubernetesClusterInfo,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    controller::dereference::DereferencedObjects,
    crd::{
        HTTPS_PORT, NifiConfig, NifiRole, sensitive_properties,
        sensitive_properties::NifiSensitiveKeyAlgorithm, v1alpha1,
    },
    framework::role_utils::with_validated_config,
    reporting_task,
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

    #[snafu(display("invalid NiFi authentication configuration"))]
    InvalidAuthenticationConfig { source: authentication::Error },

    #[snafu(display("failed to build reporting task service name"))]
    ReportingTask {
        source: crate::reporting_task::Error,
    },

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
/// in fail-safe / resolved form. The raw `NifiCluster` should only be needed for
/// OwnerReferences after this point.
pub struct ValidatedCluster {
    #[allow(dead_code)]
    pub name: String,
    pub image: ResolvedProductImage,
    pub role_group_configs: BTreeMap<NifiRole, BTreeMap<String, NifiRoleGroupConfig>>,
    pub cluster_config: ValidatedClusterConfig,
}

pub struct ValidatedClusterConfig {
    pub authentication: NifiAuthenticationConfig,
    pub authorization: ResolvedNifiAuthorizationConfig,
    /// Comma-separated NiFi proxy hosts, or `"*"` if `hostHeaderCheck.allowAll` is set.
    pub proxy_hosts: String,
    /// The clustering backend (ZooKeeper or Kubernetes), copied from the spec.
    pub clustering_backend: v1alpha1::NifiClusteringBackend,
    /// The validated sensitive properties algorithm.
    pub sensitive_properties_algorithm: NifiSensitiveKeyAlgorithm,
}

/// Validates the cluster spec and the dereferenced inputs.
pub fn validate(
    nifi: &v1alpha1::NifiCluster,
    dereferenced_objects: &DereferencedObjects,
    operator_environment: &OperatorEnvironmentOptions,
    cluster_info: &KubernetesClusterInfo,
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

    let proxy_hosts = compute_proxy_hosts(nifi, cluster_info)?;

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

    Ok(ValidatedCluster {
        name: nifi.name_any(),
        image,
        role_group_configs: build_role_group_configs(nifi)?,
        cluster_config: ValidatedClusterConfig {
            authentication: authentication_config,
            authorization: authorization_config,
            proxy_hosts,
            clustering_backend: nifi.spec.cluster_config.clustering_backend.clone(),
            sensitive_properties_algorithm,
        },
    })
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

fn compute_proxy_hosts(
    nifi: &v1alpha1::NifiCluster,
    cluster_info: &KubernetesClusterInfo,
) -> Result<String> {
    let host_header_check = &nifi.spec.cluster_config.host_header_check;

    if host_header_check.allow_all {
        tracing::info!(
            "spec.clusterConfig.hostHeaderCheck.allowAll is set to true. All proxy hosts will be allowed."
        );
        if !host_header_check.additional_allowed_hosts.is_empty() {
            tracing::info!(
                "spec.clusterConfig.hostHeaderCheck.additionalAllowedHosts is ignored and only '*' is added to the allow-list."
            )
        }
        return Ok("*".to_string());
    }

    // Address and port are injected from the listener volume during the prepare container
    let mut proxy_hosts = HashSet::from([
        "${env:LISTENER_DEFAULT_ADDRESS}:${env:LISTENER_DEFAULT_PORT_HTTPS}".to_string(),
    ]);
    proxy_hosts.extend(host_header_check.additional_allowed_hosts.iter().cloned());

    // Reporting task only exists for NiFi 1.x
    if nifi.spec.image.product_version().starts_with("1.") {
        let reporting_task_service_name =
            reporting_task::build_reporting_task_fqdn_service_name(nifi, cluster_info)
                .context(ReportingTaskSnafu)?;

        proxy_hosts.insert(format!("{reporting_task_service_name}:{HTTPS_PORT}"));
    }

    let mut proxy_hosts = Vec::from_iter(proxy_hosts);
    proxy_hosts.sort();

    Ok(proxy_hosts.join(","))
}
