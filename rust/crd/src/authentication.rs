use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_operator::commons::authentication::AuthenticationClassProvider;
use stackable_operator::kube::ResourceExt;
use stackable_operator::{
    client::Client,
    commons::authentication::AuthenticationClass,
    kube::runtime::reflector::ObjectRef,
    schemars::{self, JsonSchema},
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("Failed to retrieve AuthenticationClass {authentication_class}"))]
    AuthenticationClassRetrieval {
        source: stackable_operator::client::Error,
        authentication_class: ObjectRef<AuthenticationClass>,
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
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiAuthenticationClassRef {
    /// Name of the [AuthenticationClass](DOCS_BASE_URL_PLACEHOLDER/concepts/authentication) used to authenticate users.
    /// Supported providers are `static` and `ldap`.
    /// For `static` the "admin" user needs to be present in the referenced secret, and only this user will be added to NiFi, other users are ignored.
    pub authentication_class: String,
}

/// Retrieve all provided `AuthenticationClass` references.
pub async fn resolve_authentication_classes(
    client: &Client,
    authentication_class_refs: &Vec<NifiAuthenticationClassRef>,
) -> Result<Vec<AuthenticationClass>> {
    let mut resolved_auth_classes = vec![];

    match authentication_class_refs.len() {
        0 => NoAuthenticationNotSupportedSnafu.fail()?,
        1 => {}
        _ => MultipleAuthenticationClassesNotSupportedSnafu.fail()?,
    }

    for auth_class in authentication_class_refs {
        let resolved_auth_class =
            AuthenticationClass::resolve(client, &auth_class.authentication_class)
                .await
                .context(AuthenticationClassRetrievalSnafu {
                    authentication_class: ObjectRef::<AuthenticationClass>::new(
                        &auth_class.authentication_class,
                    ),
                })?;

        let resolved_auth_class_name = resolved_auth_class.name_any();

        match &resolved_auth_class.spec.provider {
            AuthenticationClassProvider::Static(_) => {}
            AuthenticationClassProvider::Ldap(ldap) => {
                if ldap.tls.uses_tls() && !ldap.tls.uses_tls_verification() {
                    NoLdapTlsVerificationNotSupportedSnafu {
                        authentication_class: ObjectRef::<AuthenticationClass>::new(
                            &resolved_auth_class_name,
                        ),
                    }
                    .fail()?
                }
            }
            _ => AuthenticationClassProviderNotSupportedSnafu {
                authentication_class_provider: resolved_auth_class.spec.provider.to_string(),
                authentication_class: ObjectRef::<AuthenticationClass>::new(
                    &resolved_auth_class_name,
                ),
            }
            .fail()?,
        };

        resolved_auth_classes.push(resolved_auth_class);
    }

    Ok(resolved_auth_classes)
}
