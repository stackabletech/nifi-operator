use std::collections::BTreeMap;

use rand::{RngExt, distr::Alphanumeric};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    commons::tls_verification::{CaCert, TlsServerVerification, TlsVerification},
    crd::authentication::oidc,
    k8s_openapi::{ByteString, api::core::v1::Secret},
    kube::ResourceExt,
};

use crate::{crd::v1alpha1, security::authentication::STACKABLE_ADMIN_USERNAME};

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("invalid well-known OIDC configuration URL"))]
    InvalidWellKnownConfigUrl {
        source: stackable_operator::crd::authentication::oidc::v1alpha1::Error,
    },

    #[snafu(display("Nifi doesn't support skipping the OIDC TLS verification"))]
    SkippingTlsVerificationNotSupported {},
}

/// Returns a password to be used by the OIDC admin user.
/// If the Secret containing the password already exists and contains the expected key, the existing password is returned.
/// Otherwise a new random password is generated.
pub(crate) fn build_oidc_admin_password_secret(oidc_admin_secret: Option<Secret>) -> String {
    match oidc_admin_secret {
        Some(secret) => {
            let existing_password = secret
                .data
                .as_ref()
                .and_then(|data| data.get(STACKABLE_ADMIN_USERNAME))
                .map(decode_admin_password);

            match existing_password {
                Some(password) => password,
                None => {
                    tracing::info!(
                        expected_key = STACKABLE_ADMIN_USERNAME,
                        "Found existing OIDC admin password secret, but it doesn't contain the expected key, generating new password"
                    );
                    encode_admin_password(15)
                }
            }
        }
        None => {
            tracing::info!("No existing OIDC admin password secret found, generating new one");
            encode_admin_password(15)
        }
    }
}

// TODO: maybe switch to get_random_base64() (not public atm) from op-rs which is ASCII clean and thus more suitable for passwords:
// https://github.com/stackabletech/operator-rs/blob/main/crates/stackable-operator/src/commons/random_secret_creation.rs#L127-L127
fn encode_admin_password(size_bytes: usize) -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(size_bytes)
        .map(char::from)
        .collect()
}

fn decode_admin_password(encoded: &ByteString) -> String {
    String::from_utf8_lossy(&encoded.0).into_owned()
}

pub(crate) fn build_oidc_admin_password_secret_name(nifi: &v1alpha1::NifiCluster) -> String {
    format!("{}-oidc-admin-password", nifi.name_any())
}

/// Adds all the required configuration properties to enable OIDC authentication.
pub fn add_oidc_config_to_properties(
    provider: &oidc::v1alpha1::AuthenticationProvider,
    client_auth_options: &oidc::v1alpha1::ClientAuthenticationOptions,
    properties: &mut BTreeMap<String, String>,
) -> Result<(), Error> {
    let well_known_url = provider
        .well_known_config_url()
        .context(InvalidWellKnownConfigUrlSnafu)?;

    properties.insert(
        "nifi.security.user.oidc.discovery.url".to_string(),
        well_known_url.to_string(),
    );
    let (oidc_client_id_env, oidc_client_secret_env) =
        oidc::v1alpha1::AuthenticationProvider::client_credentials_env_names(
            &client_auth_options.client_credentials_secret_ref,
        );
    properties.insert(
        "nifi.security.user.oidc.client.id".to_string(),
        format!("${{env:{oidc_client_id_env}}}").to_string(),
    );
    properties.insert(
        "nifi.security.user.oidc.client.secret".to_string(),
        format!("${{env:{oidc_client_secret_env}}}").to_string(),
    );
    let scopes = provider.scopes.join(",");
    properties.insert(
        "nifi.security.user.oidc.additional.scopes".to_string(),
        scopes.to_string(),
    );
    properties.insert(
        "nifi.security.user.oidc.claim.identifying.user".to_string(),
        provider.principal_claim.to_string(),
    );

    if let Some(tls) = &provider.tls.tls {
        let truststore_strategy = match tls.verification {
            TlsVerification::None {} => SkippingTlsVerificationNotSupportedSnafu.fail()?,
            TlsVerification::Server(TlsServerVerification {
                ca_cert: CaCert::SecretClass(_),
            }) => "NIFI", // The cert get's added to the stackable truststore
            TlsVerification::Server(TlsServerVerification {
                ca_cert: CaCert::WebPki {},
            }) => "JDK", // The cert needs to be in the system truststore
        };
        properties.insert(
            "nifi.security.user.oidc.truststore.strategy".to_owned(),
            truststore_strategy.to_owned(),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use stackable_operator::commons::tls_verification::{Tls, TlsClientDetails};

    use super::*;

    #[rstest]
    #[case("/realms/sdp")]
    #[case("/realms/sdp/")]
    #[case("/realms/sdp/////")]
    fn test_add_oidc_config(#[case] root_path: String) {
        let mut properties = BTreeMap::new();
        let provider = oidc::v1alpha1::AuthenticationProvider::new(
            "keycloak.mycorp.org".to_owned().try_into().unwrap(),
            Some(443),
            root_path,
            TlsClientDetails {
                tls: Some(Tls {
                    verification: TlsVerification::Server(TlsServerVerification {
                        ca_cert: CaCert::WebPki {},
                    }),
                }),
            },
            "preferred_username".to_owned(),
            vec!["openid".to_owned()],
            None,
        );
        let oidc = oidc::v1alpha1::ClientAuthenticationOptions {
            client_credentials_secret_ref: "nifi-keycloak-client".to_owned(),
            extra_scopes: vec![],
            product_specific_fields: (),
        };

        add_oidc_config_to_properties(&provider, &oidc, &mut properties)
            .expect("OIDC config adding failed");

        assert_eq!(
            properties.get("nifi.security.user.oidc.additional.scopes"),
            Some(&"openid".to_owned())
        );
        assert_eq!(
            properties.get("nifi.security.user.oidc.claim.identifying.user"),
            Some(&"preferred_username".to_owned())
        );
        assert_eq!(
            properties.get("nifi.security.user.oidc.discovery.url"),
            Some(
                &"https://keycloak.mycorp.org/realms/sdp/.well-known/openid-configuration"
                    .to_owned()
            )
        );
        assert_eq!(
            properties.get("nifi.security.user.oidc.truststore.strategy"),
            Some(&"JDK".to_owned())
        );

        assert!(properties.contains_key("nifi.security.user.oidc.client.id"));
        assert!(properties.contains_key("nifi.security.user.oidc.client.secret"));
    }
}
