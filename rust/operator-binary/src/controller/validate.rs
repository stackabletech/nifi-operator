//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedInputs`], consumed by the rest of `reconcile_nifi`.

use std::collections::HashSet;

use product_config::ProductConfigManager;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection::{self, ResolvedProductImage},
    product_config_utils::ValidatedRoleConfigByPropertyKind,
    utils::cluster_info::KubernetesClusterInfo,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    config::{self, validated_product_config},
    controller::dereference::DereferencedObjects,
    crd::{HTTPS_PORT, v1alpha1},
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

    #[snafu(display("failed to load product config"))]
    ProductConfigLoadFailed {
        #[snafu(source(from(config::Error, Box::new)))]
        source: Box<config::Error>,
    },

    #[snafu(display("failed to build reporting task service name"))]
    ReportingTask {
        source: crate::reporting_task::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Synchronous inputs the rest of `reconcile_nifi` needs after dereferencing.
pub struct ValidatedInputs {
    pub image: ResolvedProductImage,
    pub authentication_config: NifiAuthenticationConfig,
    pub authorization_config: ResolvedNifiAuthorizationConfig,
    pub validated_role_config: ValidatedRoleConfigByPropertyKind,
    /// Comma-separated NiFi proxy hosts, or `"*"` if
    /// `spec.clusterConfig.hostHeaderCheck.allowAll` is set. Computed here so all derived
    /// inputs live in one place; previously this ran once per rolegroup inside the
    /// reconcile loop.
    pub proxy_hosts: String,
}

/// Validates the cluster spec and the dereferenced inputs.
pub fn validate(
    nifi: &v1alpha1::NifiCluster,
    dereferenced_objects: &DereferencedObjects,
    operator_environment: &OperatorEnvironmentOptions,
    product_config: &ProductConfigManager,
    cluster_info: &KubernetesClusterInfo,
) -> Result<ValidatedInputs> {
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

    let validated_role_config = validated_product_config(
        nifi,
        &image.product_version,
        nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?,
        product_config,
    )
    .context(ProductConfigLoadFailedSnafu)?;

    let proxy_hosts = compute_proxy_hosts(nifi, cluster_info)?;

    Ok(ValidatedInputs {
        image,
        authentication_config,
        authorization_config,
        validated_role_config,
        proxy_hosts,
    })
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
