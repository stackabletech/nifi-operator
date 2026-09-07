use std::str::FromStr;

use stackable_operator::{
    builder::pod::volume::{SecretFormat, SecretOperatorVolumeSourceBuilder, VolumeBuilder},
    commons::secret_class::SecretClassVolumeProvisionParts,
    constant,
    k8s_openapi::api::core::v1::Volume,
    shared::time::Duration,
    v2::types::kubernetes::{SecretClassName, VolumeName},
};

use crate::security::authentication::STACKABLE_TLS_STORE_PASSWORD;

constant!(pub KEYSTORE_VOLUME_NAME: VolumeName = "keystore");
pub const KEYSTORE_NIFI_CONTAINER_MOUNT: &str = "/stackable/keystore";
constant!(pub TRUSTSTORE_VOLUME_NAME: VolumeName = "truststore");

/// Builds the secret-operator volume providing the TLS keystore for the given SecretClass.
///
/// # Panics
///
/// Panics if the volume source cannot be built, which cannot happen because the annotation
/// keys are static and annotation values cannot be invalid.
pub(crate) fn build_tls_volume(
    server_tls_secret_class: &SecretClassName,
    volume_name: &VolumeName,
    service_scopes: impl IntoIterator<Item = impl AsRef<str>>,
    secret_format: SecretFormat,
    requested_secret_lifetime: &Duration,
    listener_scope: Option<&str>,
) -> Volume {
    let mut secret_volume_source_builder = SecretOperatorVolumeSourceBuilder::new(
        server_tls_secret_class,
        // NiFi serves its own TLS endpoints, so the Pod needs both the public
        // certificate and the private key.
        SecretClassVolumeProvisionParts::PublicPrivate,
    );

    if secret_format == SecretFormat::TlsPkcs12 {
        secret_volume_source_builder.with_tls_pkcs12_password(STACKABLE_TLS_STORE_PASSWORD);
    }
    for scope in service_scopes {
        secret_volume_source_builder.with_service_scope(scope.as_ref());
    }
    if let Some(listener_scope) = listener_scope {
        secret_volume_source_builder.with_listener_volume_scope(listener_scope);
    }

    VolumeBuilder::new(volume_name)
        .ephemeral(
            secret_volume_source_builder
                .with_pod_scope()
                .with_format(secret_format)
                .with_auto_tls_cert_lifetime(*requested_secret_lifetime)
                .build()
                .expect("The annotation keys are static and annotation values cannot be invalid."),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *KEYSTORE_VOLUME_NAME;
        let _ = *TRUSTSTORE_VOLUME_NAME;
    }
}
