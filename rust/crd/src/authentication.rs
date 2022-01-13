use crate::authentication::NifiAuthenticationMethodReference::SingleUser;
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use snafu::{OptionExt, Snafu};
use stackable_operator::client::Client;
use stackable_operator::k8s_openapi::api::core::v1::{Secret, SecretReference};
use stackable_operator::k8s_openapi::ByteString;
use stackable_operator::schemars::{self, JsonSchema};
use std::collections::BTreeMap;
use std::string::FromUtf8Error;
use xml::escape::escape_str_attribute;

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("Failed to find referenced secret [{}/{}]", name, namespace))]
    MissingSecret {
        source: stackable_operator::error::Error,
        name: String,
        namespace: String,
    },
    #[snafu(display("Missing mandatory configuration key [{}] when parsing secret", key))]
    MissingKey { key: String },
    #[snafu(display(
        "Missing mandatory secret reference when parsing authentication configuration: [{}]",
        secret
    ))]
    MissingSecretReference { secret: String },
    #[snafu(display(
        "A required value was not found when parsing the authentication config: [{}]",
        value
    ))]
    MissingRequiredValue { value: String },
    #[snafu(display(
        "Unable to convert from Utf8 to String when reading Secret: [{}]",
        value
    ))]
    Utf8Error {
        source: FromUtf8Error,
        value: String,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationConfig<T> {
    pub method: T,
    pub config: Option<BTreeMap<String, String>>,
    pub secrets: Option<BTreeMap<String, SecretReference>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, strum::Display)]
#[strum(serialize_all = "camelCase")]
pub enum NifiAuthenticationMethod {
    SingleUser,
}

#[derive(strum::Display, Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiAuthenticationMethodReference {
    Nothing,
    SingleUser {
        admin_user_reference: SecretReference,
    },
}

#[derive(strum::Display, Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiAuthenticationMethodConfig {
    Nothing,
    SingleUser { username: String, password: String },
}

pub async fn build_auth_reference(
    config: &AuthenticationConfig<NifiAuthenticationMethod>,
) -> Result<NifiAuthenticationMethodReference, Error> {
    match config.method {
        NifiAuthenticationMethod::SingleUser => {
            let secrets = config
                .secrets
                .as_ref()
                .with_context(|| MissingSecretReference {
                    secret: "secrets".to_string(),
                })?
                .to_owned();
            let user_secret =
                secrets
                    .get("admincredentials")
                    .with_context(|| MissingSecretReference {
                        secret: "admincredentials".to_string(),
                    })?;
            Ok(NifiAuthenticationMethodReference::SingleUser {
                admin_user_reference: user_secret.to_owned(),
            })
        }
    }
}

pub async fn materialize_auth_config(
    client: &Client,
    reference: &NifiAuthenticationMethodReference,
) -> Result<NifiAuthenticationMethodConfig, Error> {
    match reference {
        SingleUser {
            admin_user_reference,
        } => {
            let secret_name = admin_user_reference.name.as_deref().unwrap();
            let secret_namespace = admin_user_reference.namespace.as_deref();

            let secret_content = client
                .get::<Secret>(secret_name, secret_namespace)
                .await
                .with_context(|| MissingSecret {
                    name: secret_name.to_string(),
                    namespace: secret_namespace.unwrap_or("Undefined"),
                })?;
            let data = &secret_content.data.with_context(|| MissingRequiredValue {
                value: "admincredentials secret contains no data".to_string(),
            })?;
            build_single_user_config(Some(reference), data)
        }
        _ => Ok(NifiAuthenticationMethodConfig::Nothing),
    }
}

pub fn get_authorizer_xml(config: &NifiAuthenticationMethodConfig) -> String {
    match config {
        NifiAuthenticationMethodConfig::Nothing => "".to_string(),
        NifiAuthenticationMethodConfig::SingleUser { username, password } => {
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>
     <loginIdentityProviders>
        <provider>
            <identifier>single-user-provider</identifier>
            <class>org.apache.nifi.authentication.single.user.SingleUserLoginIdentityProvider</class>
            <property name=\"Username\">{}</property>
            <property name=\"Password\">{}</property>
        </provider>
     </loginIdentityProviders>", escape_str_attribute(username), escape_str_attribute(password))
        }
    }
}

fn build_single_user_config(
    _reference: Option<&NifiAuthenticationMethodReference>,
    secret_data: &BTreeMap<String, ByteString>,
) -> Result<NifiAuthenticationMethodConfig, Error> {
    let username = String::from_utf8(
        secret_data
            .get("username")
            .with_context(|| MissingKey {
                key: "username".to_string(),
            })?
            .to_owned()
            .0,
    )
    .with_context(|| Utf8Error {
        value: "admincredentials.username".to_string(),
    })?;

    let password = String::from_utf8(
        secret_data
            .get("password")
            .with_context(|| MissingKey {
                key: "password".to_string(),
            })?
            .to_owned()
            .0,
    )
    .with_context(|| Utf8Error {
        value: "admincredentials.username".to_string(),
    })?;

    Ok(NifiAuthenticationMethodConfig::SingleUser { username, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_missing_fails() {
        let mut secret_data: BTreeMap<String, ByteString> = BTreeMap::new();
        secret_data.insert(
            "password".to_string(),
            ByteString {
                0: "test".as_bytes().to_vec(),
            },
        );

        let result = build_single_user_config(None, &secret_data);

        assert!(result.is_err());
    }

    #[test]
    fn test_user_missing_fails() {
        let mut secret_data: BTreeMap<String, ByteString> = BTreeMap::new();
        secret_data.insert(
            "username".to_string(),
            ByteString {
                0: "test".as_bytes().to_vec(),
            },
        );

        let result = build_single_user_config(None, &secret_data);

        assert!(result.is_err());
    }

    #[test]
    fn test_success() {
        let mut secret_data: BTreeMap<String, ByteString> = BTreeMap::new();
        secret_data.insert(
            "username".to_string(),
            ByteString {
                0: "test".as_bytes().to_vec(),
            },
        );
        secret_data.insert(
            "password".to_string(),
            ByteString {
                0: "testpassword".as_bytes().to_vec(),
            },
        );

        let result = build_single_user_config(None, &secret_data).unwrap();

        match result {
            NifiAuthenticationMethodConfig::SingleUser { username, password } => {
                assert_eq!(username, "test".to_string());
                assert_eq!(password, "testpassword");
            }
            _ => {
                assert!(false)
            }
        }
    }

    #[test]
    fn test_success_with_excess_keys() {
        let mut secret_data: BTreeMap<String, ByteString> = BTreeMap::new();
        secret_data.insert(
            "username".to_string(),
            ByteString {
                0: "test".as_bytes().to_vec(),
            },
        );
        secret_data.insert(
            "password".to_string(),
            ByteString {
                0: "testpassword".as_bytes().to_vec(),
            },
        );
        secret_data.insert(
            "unneeded_extra_value".to_string(),
            ByteString {
                0: "testpassword".as_bytes().to_vec(),
            },
        );

        let result = build_single_user_config(None, &secret_data).unwrap();

        match result {
            NifiAuthenticationMethodConfig::SingleUser { username, password } => {
                assert_eq!(username, "test".to_string());
                assert_eq!(password, "testpassword");
            }
            _ => {
                assert!(false)
            }
        }
    }
}
