use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::client::Client;
use stackable_operator::k8s_openapi::api::core::v1::{
    Secret, SecretReference, SecretVolumeSource, Volume,
};
use stackable_operator::kube::runtime::reflector::ObjectRef;
use stackable_operator::schemars::{self, JsonSchema};
use std::collections::BTreeMap;
use std::string::FromUtf8Error;

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("Failed to find referenced secret {obj_ref}"))]
    MissingSecret {
        source: stackable_operator::error::Error,
        obj_ref: ObjectRef<Secret>,
    },
    #[snafu(display("Failed to parse utf8 string for key [{key}] in secret {obj_ref}",))]
    Utf8Failure {
        source: FromUtf8Error,
        obj_ref: ObjectRef<Secret>,
        key: String,
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
pub struct NifiAuthenticationConfig {
    pub method: NifiAuthenticationMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_anonymous_access: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, strum::Display)]
#[strum(serialize_all = "camelCase")]
pub enum NifiAuthenticationMethod {
    #[serde(rename_all = "camelCase")]
    SingleUser {
        admin_credentials_secret: SecretReference,
    },
}

impl NifiAuthenticationConfig {
    pub fn allow_anonymous(&self) -> bool {
        self.allow_anonymous_access.unwrap_or(false)
    }
}

pub async fn get_login_identity_provider_xml(
    client: &Client,
    config: &NifiAuthenticationConfig,
    current_namespace: &str,
) -> Result<String, Error> {
    match &config.method {
        NifiAuthenticationMethod::SingleUser {
            admin_credentials_secret,
        } => {
            let secret_name = admin_credentials_secret.name.clone().with_context(|| {
                MissingSecretReferenceSnafu {
                    secret: "admin_credentials_secret".to_string(),
                }
            })?;
            // If no namespace was specified the namespace of the NifiCluster object is assumed
            let secret_namespace = admin_credentials_secret
                .namespace
                .clone()
                .unwrap_or_else(|| current_namespace.to_string());
            // Get Secret content from Kube
            let secret_content: Secret = client
                .get::<Secret>(&secret_name, Some(&secret_namespace))
                .await
                .with_context(|_| MissingSecretSnafu {
                    obj_ref: ObjectRef::new(&secret_name).within(&secret_namespace),
                })?;

            let secret_data = secret_content
                .data
                .with_context(|| MissingRequiredValueSnafu {
                    value: "admin_credentials_secret contains no data".to_string(),
                })?;

            let user_name = String::from_utf8(
                secret_data
                    .get("username")
                    .with_context(|| MissingRequiredValueSnafu {
                        value: "username".to_string(),
                    })?
                    .clone()
                    .0,
            )
            .with_context(|_| Utf8FailureSnafu {
                key: "username".to_string(),
                obj_ref: ObjectRef::new(&secret_name)
                    .within(admin_credentials_secret.namespace.as_deref().unwrap_or("")),
            })?;

            let password = String::from_utf8(
                secret_data
                    .get("password")
                    .with_context(|| MissingRequiredValueSnafu {
                        value: "password".to_string(),
                    })?
                    .clone()
                    .0,
            )
            .with_context(|_| Utf8FailureSnafu {
                key: "password".to_string(),
                obj_ref: ObjectRef::new(&secret_name)
                    .within(admin_credentials_secret.namespace.as_deref().unwrap_or("")),
            })?;

            Ok(build_single_user_config(&user_name, &password))
        }
    }
}

pub fn get_auth_volumes(
    method: &NifiAuthenticationMethod,
) -> Result<BTreeMap<String, (String, Volume)>, Error> {
    match method {
        NifiAuthenticationMethod::SingleUser {
            admin_credentials_secret,
        } => {
            let mut result = BTreeMap::new();
            let admin_volume = Volume {
                name: "adminuser".to_string(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(admin_credentials_secret.name.clone().with_context(
                        || MissingRequiredValueSnafu {
                            value: "name".to_string(),
                        },
                    )?),
                    ..SecretVolumeSource::default()
                }),
                ..Volume::default()
            };
            result.insert(
                "adminuser".to_string(),
                ("/stackable/adminuser".to_string(), admin_volume),
            );
            Ok(result)
        }
    }
}

fn build_single_user_config(_username: &str, _password_hash: &str) -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>
     <loginIdentityProviders>
        <provider>
            <identifier>single-user-provider</identifier>
            <class>org.apache.nifi.authentication.single.user.SingleUserLoginIdentityProvider</class>
            <property name=\"Username\">xxx</property>
            <property name=\"Password\">yyy</property>
        </provider>
     </loginIdentityProviders>".to_string()
}
