use std::future::Future;

use snafu::{ensure, ResultExt, Snafu};
use stackable_operator::commons::authentication::oidc::IdentityProviderHint;
use stackable_operator::commons::authentication::{
    ldap, oidc, static_, AuthenticationClassProvider, ClientAuthenticationDetails,
};
use stackable_operator::kube::ResourceExt;
use stackable_operator::{
    client::Client, commons::authentication::AuthenticationClass,
    kube::runtime::reflector::ObjectRef,
};
use tracing::info;

use crate::NifiCluster;

// The assumed OIDC provider if no hint is given in the AuthClass
pub const DEFAULT_OIDC_PROVIDER: IdentityProviderHint = IdentityProviderHint::Keycloak;

const SUPPORTED_OIDC_PROVIDERS: &[IdentityProviderHint] = &[IdentityProviderHint::Keycloak];

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to retrieve AuthenticationClass"))]
    AuthenticationClassRetrievalFailed {
        source: stackable_operator::client::Error,
    },

    #[snafu(display("The nifi-operator does not support running Nifi without any authentication. Please provide a AuthenticationClass to use."))]
    NoAuthenticationNotSupported {},

    #[snafu(display("The nifi-operator does not support multiple AuthenticationClasses simultaneously. Please provide a single AuthenticationClass to use."))]
    MultipleAuthenticationClassesNotSupported {},

    #[snafu(display("The nifi-operator does not support the AuthenticationClass provider [{authentication_class_provider}] from AuthenticationClass [{authentication_class}]."))]
    AuthenticationClassProviderNotSupported {
        authentication_class_provider: String,
        authentication_class: ObjectRef<AuthenticationClass>,
    },

    #[snafu(display("Nifi doesn't support skipping the LDAP TLS verification of the AuthenticationClass {authentication_class}"))]
    NoLdapTlsVerificationNotSupported {
        authentication_class: ObjectRef<AuthenticationClass>,
    },
    #[snafu(display("invalid OIDC configuration"))]
    OidcConfigurationInvalid {
        source: stackable_operator::commons::authentication::Error,
    },
    #[snafu(display("the OIDC provider {oidc_provider:?} is not yet supported (AuthenticationClass {auth_class_name:?})"))]
    OidcProviderNotSupported {
        auth_class_name: String,
        oidc_provider: String,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone)]
pub enum AuthenticationClassResolved {
    Static {
        provider: static_::AuthenticationProvider,
    },
    Ldap {
        provider: ldap::AuthenticationProvider,
    },
    Oidc {
        provider: oidc::AuthenticationProvider,
        oidc: oidc::ClientAuthenticationOptions<()>,
        nifi: NifiCluster,
    },
}

impl AuthenticationClassResolved {
    pub async fn from(
        nifi: &NifiCluster,
        client: &Client,
    ) -> Result<Vec<AuthenticationClassResolved>> {
        let resolve_auth_class = |auth_details: ClientAuthenticationDetails| async move {
            auth_details.resolve_class(client).await
        };
        AuthenticationClassResolved::resolve(nifi, resolve_auth_class).await
    }

    /// Retrieve all provided `AuthenticationClass` references.
    pub async fn resolve<R>(
        nifi: &NifiCluster,
        resolve_auth_class: impl Fn(ClientAuthenticationDetails) -> R,
    ) -> Result<Vec<AuthenticationClassResolved>>
    where
        R: Future<Output = Result<AuthenticationClass, stackable_operator::client::Error>>,
    {
        let mut resolved_auth_classes = vec![];
        let auth_details = &nifi.spec.cluster_config.authentication;

        match auth_details.len() {
            0 => NoAuthenticationNotSupportedSnafu.fail()?,
            1 => {}
            _ => MultipleAuthenticationClassesNotSupportedSnafu.fail()?,
        }

        for entry in auth_details {
            let auth_class = resolve_auth_class(entry.clone())
                .await
                .context(AuthenticationClassRetrievalFailedSnafu)?;

            let auth_class_name = auth_class.name_any();

            match &auth_class.spec.provider {
                AuthenticationClassProvider::Static(provider) => {
                    resolved_auth_classes.push(AuthenticationClassResolved::Static {
                        provider: provider.to_owned(),
                    })
                }
                AuthenticationClassProvider::Ldap(provider) => {
                    if provider.tls.uses_tls() && !provider.tls.uses_tls_verification() {
                        NoLdapTlsVerificationNotSupportedSnafu {
                            authentication_class: ObjectRef::<AuthenticationClass>::new(
                                &auth_class_name,
                            ),
                        }
                        .fail()?
                    }
                    resolved_auth_classes.push(AuthenticationClassResolved::Ldap {
                        provider: provider.to_owned(),
                    })
                }
                AuthenticationClassProvider::Oidc(provider) => {
                    resolved_auth_classes.push(AuthenticationClassResolved::from_oidc(
                        &auth_class_name,
                        provider,
                        entry,
                        nifi.clone(),
                    )?)
                }
                _ => AuthenticationClassProviderNotSupportedSnafu {
                    authentication_class_provider: auth_class.spec.provider.to_string(),
                    authentication_class: ObjectRef::<AuthenticationClass>::new(&auth_class_name),
                }
                .fail()?,
            };
        }

        Ok(resolved_auth_classes)
    }

    fn from_oidc(
        auth_class_name: &str,
        provider: &oidc::AuthenticationProvider,
        auth_details: &ClientAuthenticationDetails,
        nifi: NifiCluster,
    ) -> Result<AuthenticationClassResolved> {
        let oidc_provider = match &provider.provider_hint {
            None => {
                info!("No OIDC provider hint given in AuthClass {auth_class_name}, assuming {default_oidc_provider_name}",
                    default_oidc_provider_name = serde_json::to_string(&DEFAULT_OIDC_PROVIDER).unwrap());
                DEFAULT_OIDC_PROVIDER
            }
            Some(oidc_provider) => oidc_provider.to_owned(),
        };

        ensure!(
            SUPPORTED_OIDC_PROVIDERS.contains(&oidc_provider),
            OidcProviderNotSupportedSnafu {
                auth_class_name,
                oidc_provider: serde_json::to_string(&oidc_provider).unwrap(),
            }
        );

        Ok(AuthenticationClassResolved::Oidc {
            provider: provider.to_owned(),
            oidc: auth_details
                .oidc_or_error(auth_class_name)
                .context(OidcConfigurationInvalidSnafu)?
                .clone(),
            nifi: nifi,
        })
    }
}
