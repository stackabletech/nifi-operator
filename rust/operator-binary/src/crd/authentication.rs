use std::future::Future;

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    client::Client,
    crd::authentication::{core as auth_core, ldap, oidc, r#static},
    kube::{ResourceExt, runtime::reflector::ObjectRef},
};

use crate::crd::v1alpha1;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to retrieve AuthenticationClass"))]
    AuthenticationClassRetrievalFailed {
        source: stackable_operator::client::Error,
    },

    #[snafu(display(
        "The nifi-operator does not support running Nifi without any authentication. Please provide a AuthenticationClass to use."
    ))]
    NoAuthenticationNotSupported {},

    #[snafu(display(
        "The nifi-operator does not support multiple AuthenticationClasses simultaneously. Please provide a single AuthenticationClass to use."
    ))]
    MultipleAuthenticationClassesNotSupported {},

    #[snafu(display(
        "The nifi-operator does not support the AuthenticationClass provider [{authentication_class_provider}] from AuthenticationClass [{authentication_class}]."
    ))]
    AuthenticationClassProviderNotSupported {
        authentication_class_provider: String,
        authentication_class: ObjectRef<auth_core::v1alpha1::AuthenticationClass>,
    },

    #[snafu(display(
        "Nifi doesn't support skipping the LDAP TLS verification of the AuthenticationClass {authentication_class}"
    ))]
    NoLdapTlsVerificationNotSupported {
        authentication_class: ObjectRef<auth_core::v1alpha1::AuthenticationClass>,
    },

    #[snafu(display("invalid OIDC configuration"))]
    OidcConfigurationInvalid {
        source: stackable_operator::crd::authentication::core::v1alpha1::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum AuthenticationClassResolved {
    Static {
        provider: r#static::v1alpha1::AuthenticationProvider,
    },
    Ldap {
        provider: ldap::v1alpha1::AuthenticationProvider,
    },
    Oidc {
        provider: oidc::v1alpha1::AuthenticationProvider,
        oidc: oidc::v1alpha1::ClientAuthenticationOptions<()>,
        // NOTE (@NickLarsenNZ): This is causing clippy::large_enum_variant
        // can we box it?
        nifi: v1alpha1::NifiCluster,
    },
}

impl AuthenticationClassResolved {
    pub async fn from(
        nifi: &v1alpha1::NifiCluster,
        client: &Client,
    ) -> Result<Vec<AuthenticationClassResolved>> {
        let resolve_auth_class =
            |auth_details: auth_core::v1alpha1::ClientAuthenticationDetails| async move {
                auth_details.resolve_class(client).await
            };
        AuthenticationClassResolved::resolve(nifi, resolve_auth_class).await
    }

    /// Retrieve all provided `AuthenticationClass` references.
    pub async fn resolve<R>(
        nifi: &v1alpha1::NifiCluster,
        resolve_auth_class: impl Fn(auth_core::v1alpha1::ClientAuthenticationDetails) -> R,
    ) -> Result<Vec<AuthenticationClassResolved>>
    where
        R: Future<
            Output = Result<
                auth_core::v1alpha1::AuthenticationClass,
                stackable_operator::client::Error,
            >,
        >,
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
                auth_core::v1alpha1::AuthenticationClassProvider::Static(provider) => {
                    resolved_auth_classes.push(AuthenticationClassResolved::Static {
                        provider: provider.to_owned(),
                    })
                }
                auth_core::v1alpha1::AuthenticationClassProvider::Ldap(provider) => {
                    if provider.tls.uses_tls() && !provider.tls.uses_tls_verification() {
                        NoLdapTlsVerificationNotSupportedSnafu {
                            authentication_class: ObjectRef::<
                                auth_core::v1alpha1::AuthenticationClass,
                            >::new(
                                &auth_class_name
                            ),
                        }
                        .fail()?
                    }
                    resolved_auth_classes.push(AuthenticationClassResolved::Ldap {
                        provider: provider.to_owned(),
                    })
                }
                auth_core::v1alpha1::AuthenticationClassProvider::Oidc(provider) => {
                    resolved_auth_classes.push(Ok(AuthenticationClassResolved::Oidc {
                        provider: provider.to_owned(),
                        oidc: entry
                            .oidc_or_error(&auth_class_name)
                            .context(OidcConfigurationInvalidSnafu)?
                            .clone(),
                        nifi: nifi.clone(),
                    })?)
                }
                _ => {
                    AuthenticationClassProviderNotSupportedSnafu {
                        authentication_class_provider: auth_class.spec.provider.to_string(),
                        authentication_class:
                            ObjectRef::<auth_core::v1alpha1::AuthenticationClass>::new(
                                &auth_class_name,
                            ),
                    }
                    .fail()?
                }
            };
        }

        Ok(resolved_auth_classes)
    }
}
