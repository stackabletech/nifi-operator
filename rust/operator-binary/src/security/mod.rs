use snafu::{ResultExt, Snafu};
use stackable_nifi_crd::NifiCluster;
use stackable_operator::client::Client;
use stackable_operator::{builder::pod::volume::SecretFormat, k8s_openapi::api::core::v1::Volume};

pub mod authentication;
pub mod sensitive_key;
pub mod tls;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("tls failure"))]
    Tls { source: tls::Error },

    #[snafu(display("sensistive key failure"))]
    SensitiveKey { source: sensitive_key::Error },
}

pub async fn check_or_generate_sensitive_key(client: &Client, nifi: &NifiCluster) -> Result<bool> {
    sensitive_key::check_or_generate_sensitive_key(client, nifi)
        .await
        .context(SensitiveKeySnafu)
}

pub fn build_tls_volume(
    volume_name: &str,
    service_scopes: Vec<&str>,
    secret_format: SecretFormat,
) -> Result<Volume> {
    tls::build_tls_volume(volume_name, service_scopes, secret_format).context(TlsSnafu)
}
