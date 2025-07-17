use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::pod::volume::SecretFormat, client::Client, k8s_openapi::api::core::v1::Volume,
    time::Duration,
};

use crate::crd::v1alpha1;

pub mod authentication;
pub mod authorization;
pub mod oidc;
pub mod sensitive_key;
pub mod tls;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("tls failure"))]
    Tls { source: tls::Error },

    #[snafu(display("sensistive key failure"))]
    SensitiveKey { source: sensitive_key::Error },

    #[snafu(display("failed to ensure OIDC admin password exists"))]
    OidcAdminPassword { source: oidc::Error },
}

pub async fn check_or_generate_sensitive_key(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<bool> {
    sensitive_key::check_or_generate_sensitive_key(client, nifi)
        .await
        .context(SensitiveKeySnafu)
}

pub async fn check_or_generate_oidc_admin_password(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<bool> {
    oidc::check_or_generate_oidc_admin_password(client, nifi)
        .await
        .context(OidcAdminPasswordSnafu)
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
