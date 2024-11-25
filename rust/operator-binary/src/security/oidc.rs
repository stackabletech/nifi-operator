use std::collections::BTreeMap;

use rand::{distributions::Alphanumeric, Rng};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::NifiCluster;
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    client::Client,
    commons::{
        authentication::oidc::{self, AuthenticationProvider, ClientAuthenticationOptions},
        tls_verification::{CaCert, TlsServerVerification, TlsVerification},
    },
    k8s_openapi::api::core::v1::Secret,
    kube::{runtime::reflector::ObjectRef, ResourceExt},
};

use super::authentication::STACKABLE_ADMIN_USERNAME;

const STACKABLE_OIDC_ADMIN_PASSWORD_KEY: &str = STACKABLE_ADMIN_USERNAME;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("the NiFi object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("failed to fetch or create OIDC admin password secret"))]
    OidcAdminPasswordSecret {
        source: stackable_operator::client::Error,
    },

    #[snafu(display(
        "found existing admin password secret {secret:?}, but the key {STACKABLE_OIDC_ADMIN_PASSWORD_KEY} is missing",
    ))]
    MissingAdminPasswordKey { secret: ObjectRef<Secret> },

    #[snafu(display("invalid well-known OIDC configuration URL"))]
    InvalidWellKnownConfigUrl {
        source: stackable_operator::commons::authentication::oidc::Error,
    },

    #[snafu(display("Nifi doesn't support skipping the OIDC TLS verification"))]
    NoOidcTlsVerificationNotSupported {},
}

/// Generate a secret containing the password for the admin user that can access the API.
///
/// This admin user is the same as for SingleUser authentication.
pub(crate) async fn check_or_generate_oidc_admin_password(
    client: &Client,
    nifi: &NifiCluster,
) -> Result<bool, Error> {
    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;
    tracing::debug!("Checking for OIDC admin password configuration");
    match client
        .get_opt::<Secret>(&build_oidc_admin_password_secret_name(nifi), namespace)
        .await
        .context(OidcAdminPasswordSecretSnafu)?
    {
        Some(secret) => {
            let admin_password_present = secret
                .data
                .iter()
                .flat_map(|data| data.keys())
                .any(|key| key == STACKABLE_OIDC_ADMIN_PASSWORD_KEY);

            if admin_password_present {
                Ok(false)
            } else {
                MissingAdminPasswordKeySnafu {
                    secret: ObjectRef::from_obj(&secret),
                }
                .fail()?
            }
        }
        None => {
            tracing::info!("No existing oidc admin password secret found, generating new one");
            let password: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(15)
                .map(char::from)
                .collect();

            let mut secret_data = BTreeMap::new();
            secret_data.insert("admin".to_string(), password);

            let new_secret = Secret {
                metadata: ObjectMetaBuilder::new()
                    .namespace(namespace)
                    .name(build_oidc_admin_password_secret_name(nifi))
                    .build(),
                string_data: Some(secret_data),
                ..Secret::default()
            };
            client
                .create(&new_secret)
                .await
                .context(OidcAdminPasswordSecretSnafu)?;
            Ok(true)
        }
    }
}

pub fn build_oidc_admin_password_secret_name(nifi: &NifiCluster) -> String {
    format!("{}-oidc-admin-password", nifi.name_any())
}

pub fn add_oidc_config(
    provider: &oidc::AuthenticationProvider,
    client_auth_options: &ClientAuthenticationOptions,
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
        AuthenticationProvider::client_credentials_env_names(
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
            TlsVerification::None {} => NoOidcTlsVerificationNotSupportedSnafu.fail()?,
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
        let provider = oidc::AuthenticationProvider::new(
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
        let oidc = ClientAuthenticationOptions {
            client_credentials_secret_ref: "nifi-keycloak-client".to_owned(),
            extra_scopes: vec![],
            product_specific_fields: (),
        };

        add_oidc_config(&provider, &oidc, &mut properties).expect("OIDC config adding failed");

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
