use crate::NifiAuthenticationMethod::SingleUser;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, Snafu};
use stackable_operator::client::Client;
use stackable_operator::k8s_openapi::api::core::v1::{Secret, SecretReference};
use stackable_operator::k8s_openapi::ByteString;
use stackable_operator::schemars::{self, JsonSchema};
use std::collections::BTreeMap;
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
    #[snafu(display("Error accessing secrets referenced in config: [{:?}]", errors))]
    SecretRetrievalError { errors: Vec<String> },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationConfig<T> {
    pub method: T,
    pub config: Option<BTreeMap<String, String>>,
    pub secrets: Option<BTreeMap<String, SecretReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_anonymous_access: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, strum::Display)]
#[strum(serialize_all = "camelCase")]
pub enum NifiAuthenticationMethod {
    SingleUser,
}

#[derive(strum::Display, Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NifiAuthenticationMethodConfig {
    SingleUser {
        username: String,
        password: String,
        allow_anonymous_access: bool,
    },
}

impl NifiAuthenticationMethodConfig {
    pub fn allow_anonymous(&self) -> &bool {
        match self {
            NifiAuthenticationMethodConfig::SingleUser {
                username: _username,
                password: _password,
                allow_anonymous_access,
            } => allow_anonymous_access,
        }
    }
}

/// This is the main entrypoint into this module and should be called from the operator to
/// generate a fully qualified authentication enum variant which can be used to generate the actual
/// config later on.
pub async fn materialize_auth_config(
    client: &Client,
    reference: &AuthenticationConfig<NifiAuthenticationMethod>,
) -> Result<NifiAuthenticationMethodConfig, Error> {
    // Retrieve data for secrets referenced in config
    let secret_data = match &reference.secrets {
        None => BTreeMap::new(),
        Some(secret_list) => get_secret_data(client, secret_list).await?,
    };

    // Build config object from secret data
    build_config(reference, &secret_data)
}

/// Takes a list of SecretReferences and retrieves the data stored in these secrets from Kubernetes
/// Returns an Error containing a list of things that went wrong, or a list of Strings.
pub async fn get_secret_data(
    client: &Client,
    secret_list: &BTreeMap<String, SecretReference>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, Error> {
    let mut result: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut error_list: Vec<String> = Vec::new();

    // Get and unwrap secret name and namespace from SecretReference
    for (secret_key, secret_reference) in secret_list.iter() {
        let secret_name = match &secret_reference.name {
            Some(secret_name) => secret_name,
            None => {
                error_list.push(format!(
                    "Field \"name\" not populated for secret with key [{}])",
                    secret_key
                ));
                continue;
            }
        };
        let secret_namespace = secret_reference.namespace.as_deref();

        // Get Secret content from Kube
        let secret_content: Secret = match client.get::<Secret>(secret_name, secret_namespace).await
        {
            Ok(content) => content,
            Err(e) => {
                error_list.push(format!(
                    "Got error when retrieving secret from Kubernetes: [{:?}]",
                    e
                ));
                continue;
            }
        };

        let data: BTreeMap<String, ByteString> = match &secret_content.data {
            Some(secret_data) => secret_data.to_owned(),
            None => {
                error_list.push(format!(
                    "Secret [{}/{}] referenced by config key [{}] does not contain any data.",
                    secret_name,
                    secret_namespace.unwrap_or(""),
                    secret_key
                ));
                continue;
            }
        };

        result.insert(
            secret_key.to_owned(),
            data.into_iter()
                .map(|(key, value)| (key, String::from_utf8(value.0).unwrap()))
                .collect::<BTreeMap<String, String>>(),
        );
    }

    if error_list.is_empty() {
        Ok(result)
    } else {
        Err(Error::SecretRetrievalError { errors: error_list })
    }
}

pub fn get_authorizer_xml(config: &NifiAuthenticationMethodConfig) -> String {
    match config {
        NifiAuthenticationMethodConfig::SingleUser {
            username, password, ..
        } => {
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

fn build_config(
    reference: &AuthenticationConfig<NifiAuthenticationMethod>,
    secret_data: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<NifiAuthenticationMethodConfig, Error> {
    match reference.method {
        SingleUser => {
            let admin_credential_secret_data =
                secret_data
                    .get("admincredentials")
                    .with_context(|| MissingSecretReference {
                        secret: "admincredentials".to_string(),
                    })?;

            let username = admin_credential_secret_data
                .get("username")
                .with_context(|| MissingKey {
                    key: "username".to_string(),
                })?
                .to_owned();

            let password = admin_credential_secret_data
                .get("password")
                .with_context(|| MissingKey {
                    key: "password".to_string(),
                })?
                .to_owned();

            Ok(NifiAuthenticationMethodConfig::SingleUser {
                username,
                password,
                allow_anonymous_access: reference.allow_anonymous_access.unwrap_or(false),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_default_config() -> AuthenticationConfig<NifiAuthenticationMethod> {
        AuthenticationConfig {
            method: NifiAuthenticationMethod::SingleUser,
            config: None,
            secrets: None,
        }
    }

    #[test]
    fn test_password_missing_fails() {
        let mut secret_data: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut admin_credential_data: BTreeMap<String, String> = BTreeMap::new();

        admin_credential_data.insert("password".to_string(), "test".to_string());
        secret_data.insert("admincredentials".to_string(), admin_credential_data);

        let result = build_config(&get_default_config(), &secret_data);

        assert!(result.is_err());
    }

    #[test]
    fn test_user_missing_fails() {
        let mut secret_data: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut admin_credential_data: BTreeMap<String, String> = BTreeMap::new();

        admin_credential_data.insert("username".to_string(), "test".to_string());
        secret_data.insert("admincredentials".to_string(), admin_credential_data);

        let result = build_config(&get_default_config(), &secret_data);

        assert!(result.is_err());
    }

    #[test]
    fn test_success() {
        let mut secret_data: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut admin_credential_data: BTreeMap<String, String> = BTreeMap::new();

        admin_credential_data.insert("username".to_string(), "test".to_string());
        admin_credential_data.insert("password".to_string(), "testpassword".to_string());
        secret_data.insert("admincredentials".to_string(), admin_credential_data);

        let result = build_config(&get_default_config(), &secret_data).unwrap();

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
        let mut secret_data: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        let mut admin_credential_data: BTreeMap<String, String> = BTreeMap::new();

        admin_credential_data.insert("username".to_string(), "test".to_string());
        admin_credential_data.insert("password".to_string(), "testpassword".to_string());
        admin_credential_data.insert(
            "unneeded_extra_value".to_string(),
            "testpassword".to_string(),
        );

        secret_data.insert("admincredentials".to_string(), admin_credential_data);

        let result = build_config(&get_default_config(), &secret_data).unwrap();

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
