use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::pod::volume::{SecretFormat, SecretOperatorVolumeSourceBuilder, VolumeBuilder},
    k8s_openapi::api::core::v1::Volume,
    time::Duration,
};

use crate::{crd::v1alpha1, security::authentication::STACKABLE_TLS_STORE_PASSWORD};

pub const KEYSTORE_VOLUME_NAME: &str = "keystore";
pub const KEYSTORE_NIFI_CONTAINER_MOUNT: &str = "/stackable/keystore";
pub const TRUSTSTORE_VOLUME_NAME: &str = "truststore";

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build TLS certificate SecretClass Volume"))]
    TlsCertSecretClassVolumeBuild {
        source: stackable_operator::builder::pod::volume::SecretOperatorVolumeSourceBuilderError,
    },
}

pub(crate) fn build_tls_volume(
    nifi: &v1alpha1::NifiCluster,
    volume_name: &str,
    service_scopes: Vec<&str>,
    secret_format: SecretFormat,
    requested_secret_lifetime: &Duration,
    listener_scope: &str,
) -> Result<Volume> {
    let mut secret_volume_source_builder =
        SecretOperatorVolumeSourceBuilder::new(nifi.server_tls_secret_class());

    if secret_format == SecretFormat::TlsPkcs12 {
        secret_volume_source_builder.with_tls_pkcs12_password(STACKABLE_TLS_STORE_PASSWORD);
    }

    for scope in service_scopes {
        secret_volume_source_builder.with_service_scope(scope);
    }

    Ok(VolumeBuilder::new(volume_name)
        .ephemeral(
            secret_volume_source_builder
                .with_pod_scope()
                .with_listener_volume_scope(listener_scope)
                .with_format(secret_format)
                .with_auto_tls_cert_lifetime(*requested_secret_lifetime)
                .build()
                .context(TlsCertSecretClassVolumeBuildSnafu)?,
        )
        .build())
}
