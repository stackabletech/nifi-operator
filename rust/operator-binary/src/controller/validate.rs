//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require a Kubernetes client. Produces
//! [`ValidatedInputs`], consumed by the rest of `reconcile_nifi`.

use product_config::ProductConfigManager;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection::{self, ResolvedProductImage},
    product_config_utils::ValidatedRoleConfigByPropertyKind,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    config::{self, validated_product_config},
    controller::dereference::DereferencedObjects,
    crd::v1alpha1,
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
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Synchronous inputs the rest of `reconcile_nifi` needs after dereferencing.
pub struct ValidatedInputs {
    pub image: ResolvedProductImage,
    pub authentication_config: NifiAuthenticationConfig,
    pub authorization_config: ResolvedNifiAuthorizationConfig,
    pub validated_role_config: ValidatedRoleConfigByPropertyKind,
}

/// Validates the cluster spec and the dereferenced inputs.
pub fn validate(
    nifi: &v1alpha1::NifiCluster,
    dereferenced_objects: &DereferencedObjects,
    operator_environment: &OperatorEnvironmentOptions,
    product_config: &ProductConfigManager,
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

    Ok(ValidatedInputs {
        image,
        authentication_config,
        authorization_config,
        validated_role_config,
    })
}
