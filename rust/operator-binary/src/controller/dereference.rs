//! The dereference step in the NifiCluster controller
//!
//! Fetches all Kubernetes objects referenced by the NifiCluster spec and returns
//! them in [`DereferencedObjects`].

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::client::Client;

use crate::{
    crd::v1alpha1,
    security::{
        authentication::{self, DereferencedAuthenticationClasses},
        authorization::{self as authorization_mod, DereferencedAuthorization},
    },
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("failed to dereference NiFi authentication classes"))]
    DereferenceAuthenticationClasses { source: authentication::Error },

    #[snafu(display("failed to dereference NiFi authorization config"))]
    DereferenceAuthorization { source: authorization_mod::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Kubernetes objects referenced from the [`v1alpha1::NifiCluster`] spec, already fetched.
pub struct DereferencedObjects {
    pub authentication_classes: DereferencedAuthenticationClasses,
    pub authorization: DereferencedAuthorization,
}

/// Fetches all Kubernetes objects referenced from the [`v1alpha1::NifiCluster`] spec.
pub async fn dereference(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<DereferencedObjects> {
    let namespace = nifi
        .metadata
        .namespace
        .as_deref()
        .context(ObjectHasNoNamespaceSnafu)?;

    let authentication_classes = DereferencedAuthenticationClasses::dereference(nifi, client)
        .await
        .context(DereferenceAuthenticationClassesSnafu)?;

    let authorization = DereferencedAuthorization::dereference(
        &nifi.spec.cluster_config.authorization,
        client,
        namespace,
    )
    .await
    .context(DereferenceAuthorizationSnafu)?;

    Ok(DereferencedObjects {
        authentication_classes,
        authorization,
    })
}
