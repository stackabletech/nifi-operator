use std::collections::BTreeMap;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{meta::ObjectMetaBuilder, pod::volume::SecretFormat},
    client::Client,
    k8s_openapi::api::core::v1::{Secret, Volume},
    kube::ResourceExt,
    shared::time::Duration,
};

use crate::{
    crd::v1alpha1,
    security::{
        authentication::STACKABLE_ADMIN_USERNAME, oidc::build_oidc_admin_password_secret_name,
    },
};

pub mod authentication;
pub mod authorization;
pub mod oidc;
pub mod sensitive_key;
pub mod tls;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("the NiFi object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("tls failure"))]
    Tls { source: tls::Error },

    #[snafu(display("sensistive key failure"))]
    SensitiveKey { source: sensitive_key::Error },

    #[snafu(display("failed to fetch or create OIDC admin password secret"))]
    OidcAdminPasswordSecret {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("failed to build OIDC admin password secret metadata"))]
    BuildOidcAdminPasswordSecretMetadata {
        source: stackable_operator::builder::meta::Error,
    },
}

pub async fn check_or_generate_sensitive_key(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<bool> {
    sensitive_key::check_or_generate_sensitive_key(client, nifi)
        .await
        .context(SensitiveKeySnafu)
}

/// Build a Secret containing the OIDC admin password.
///
/// If the Secret object already exists and contains the expected key, the existing password is preserved.
/// Otherwise a new Secret object is created with a random password.
///
pub async fn build_oidc_admin_password_secret(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
    labels: stackable_operator::kvp::ObjectLabels<'_, v1alpha1::NifiCluster>,
) -> Result<stackable_operator::k8s_openapi::api::core::v1::Secret> {
    tracing::debug!("Checking for OIDC admin password configuration");

    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;
    let kubernetes_secret_name = build_oidc_admin_password_secret_name(nifi);

    let oidc_admin_pass_secret = client
        .get_opt::<Secret>(&kubernetes_secret_name, namespace)
        .await
        .context(OidcAdminPasswordSecretSnafu)?;

    let password = oidc::build_oidc_admin_password_secret(oidc_admin_pass_secret);

    Ok(Secret {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(build_oidc_admin_password_secret_name(nifi))
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(BuildOidcAdminPasswordSecretMetadataSnafu)?
            .with_recommended_labels(labels)
            .context(BuildOidcAdminPasswordSecretMetadataSnafu)?
            .build(),
        string_data: Some(BTreeMap::from([(
            STACKABLE_ADMIN_USERNAME.to_string(),
            password,
        )])),
        ..Secret::default()
    })
}

pub fn build_tls_volume(
    nifi: &v1alpha1::NifiCluster,
    volume_name: &str,
    service_scopes: impl IntoIterator<Item = impl AsRef<str>>,
    secret_format: SecretFormat,
    requested_secret_lifetime: &Duration,
    listener_scope: Option<&str>,
) -> Result<Volume> {
    tls::build_tls_volume(
        nifi,
        volume_name,
        service_scopes,
        secret_format,
        requested_secret_lifetime,
        listener_scope,
    )
    .context(TlsSnafu)
}
