use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use stackable_operator::builder::ObjectMetaBuilder;
use stackable_operator::client::Client;
use stackable_operator::k8s_openapi::api::core::v1::{Secret, SecretVolumeSource, Volume};
use stackable_operator::kube::runtime::reflector::ObjectRef;
use stackable_operator::schemars::{self, JsonSchema};
use std::collections::BTreeMap;

#[derive(Snafu, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("Failed to find referenced secret {obj_ref}"))]
    MissingSecret {
        source: stackable_operator::error::Error,
        obj_ref: ObjectRef<Secret>,
    },
    #[snafu(display("Error when communication with apiserver while:  {reason} "))]
    Kube {
        source: stackable_operator::error::Error,
        reason: String,
    },
    MissingSecretReference {
        secret: String,
    },
    #[snafu(display(
        "A required value was not found when parsing the authentication config: [{}]",
        value
    ))]
    MissingRequiredValue {
        value: String,
    },
    #[snafu(display(
        "Unable to load admin credentials and auto-generation is disabled: [{}]",
        message
    ))]
    AdminCredentials {
        message: String,
    },
}

pub const SINGLEUSER_DEFAULT_ADMIN: &str = "admin";
pub const SINGLEUSER_USER_KEY: &str = "username";
pub const SINGLEUSER_PASSWORD_KEY: &str = "password";

pub const AUTH_VOLUME_NAME: &str = "adminuser";
pub const AUTH_VOLUME_MOUNT_PATH: &str = "/stackable/adminuser";

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NifiAuthenticationConfig {
    pub method: NifiAuthenticationMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_anonymous_access: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "camelCase")]
pub enum NifiAuthenticationMethod {
    #[serde(rename_all = "camelCase")]
    SingleUser {
        admin_credentials_secret: String,
        #[serde(default)]
        auto_generate: bool,
    },
    #[serde(rename_all = "camelCase")]
    AuthenticationClass { authentication_class: String },
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
            auto_generate,
        } => {
            // Check if the referenced secret exists and contains all necessary keys, otherwise
            // generate random password and default user
            check_or_generate_admin_credentials(
                client,
                admin_credentials_secret,
                current_namespace,
                auto_generate,
            )
            .await?;

            Ok(include_str!(
                "../../operator-binary/resources/singleuser-login-identity-providers.xml"
            )
            .to_string())
        }
        NifiAuthenticationMethod::AuthenticationClass {
            authentication_class,
        } => todo!(),
    }
}

pub fn get_auth_volumes(
    method: &NifiAuthenticationMethod,
) -> Result<BTreeMap<String, (String, Volume)>, Error> {
    match method {
        NifiAuthenticationMethod::SingleUser {
            admin_credentials_secret,
            ..
        } => {
            let mut result = BTreeMap::new();
            let admin_volume = Volume {
                name: AUTH_VOLUME_NAME.to_string(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(admin_credentials_secret.to_string()),
                    ..SecretVolumeSource::default()
                }),
                ..Volume::default()
            };
            result.insert(
                AUTH_VOLUME_NAME.to_string(),
                (AUTH_VOLUME_MOUNT_PATH.to_string(), admin_volume),
            );
            Ok(result)
        }
        NifiAuthenticationMethod::AuthenticationClass {
            authentication_class,
        } => todo!(),
    }
}

async fn check_or_generate_admin_credentials(
    client: &Client,
    secret_name: &str,
    secret_namespace: &str,
    auto_generate: &bool,
) -> Result<bool, Error> {
    match client
        .exists::<Secret>(secret_name, Some(secret_namespace))
        .await
        .with_context(|_| KubeSnafu {
            reason: format!(
                "checking if admin credential secret exists [{}/{}]",
                secret_name, secret_namespace
            ),
        })? {
        true => {
            // The secret exists, retrieve the content and check that all required keys are present
            // any missing keys will be filled with default or generated values
            let secret_content: Secret = client
                .get::<Secret>(secret_name, Some(secret_namespace))
                .await
                .with_context(|_| MissingSecretSnafu {
                    obj_ref: ObjectRef::new(secret_name).within(secret_namespace),
                })?;

            let mut additional_data = None;
            let empty_map = BTreeMap::new();

            // Check if user key is present, otherwise add to additional data
            if !secret_content
                .data
                .as_ref()
                .unwrap_or(&empty_map)
                .contains_key(SINGLEUSER_USER_KEY)
            {
                tracing::info!(
                    "key [{}] not found in secret [{}/{}], inserting default value of \"admin\"",
                    SINGLEUSER_USER_KEY,
                    secret_name,
                    secret_namespace
                );
                additional_data.get_or_insert(BTreeMap::new()).insert(
                    SINGLEUSER_USER_KEY.to_string(),
                    SINGLEUSER_DEFAULT_ADMIN.to_string(),
                );
            }

            // Check if password key is present, otherwise add to additional data
            if !secret_content
                .data
                .as_ref()
                .unwrap_or(&empty_map)
                .contains_key(SINGLEUSER_PASSWORD_KEY)
            {
                tracing::info!(
                    "key [{}] not found in secret [{}/{}], inserting generated password",
                    SINGLEUSER_PASSWORD_KEY,
                    secret_name,
                    secret_namespace
                );
                let generated_password = rand::thread_rng()
                    .sample_iter(&Alphanumeric)
                    .take(15)
                    .map(char::from)
                    .collect();
                additional_data
                    .get_or_insert(BTreeMap::new())
                    .insert(SINGLEUSER_PASSWORD_KEY.to_string(), generated_password);
            }

            // Apply patch to secret if any additional data was needed and return
            if additional_data.is_some() {
                // Check if we are allowed to auto generate and abort if not
                if !auto_generate {
                    return Err(Error::AdminCredentials {
                        message: format!(
                            "Admin credential secret [{}/{}] is missing keys: [{:?}]",
                            secret_name,
                            secret_namespace,
                            additional_data.unwrap().keys()
                        ),
                    });
                }
                tracing::debug!(
                    "patching keys [{:?}] in secret [{}/{}]",
                    additional_data.clone().unwrap_or_default().keys(),
                    secret_name,
                    secret_namespace,
                );
                let secret_patch = Secret {
                    metadata: ObjectMetaBuilder::new()
                        .namespace(secret_namespace)
                        .name(secret_name)
                        .build(),
                    string_data: additional_data,
                    ..Secret::default()
                };
                client.apply_patch("nificluster", &secret_patch, &secret_patch).await.with_context(|_| KubeSnafu {reason: format!{"patch admin credentialsecret [{}/{}]with missing data", secret_name, secret_namespace}})?;
                Ok(true)
            } else {
                // All needed keys are present, no need to change anything
                tracing::debug!(
                    "all required data for admin credentials found in secret [{}/{}]",
                    secret_name,
                    secret_namespace
                );
                Ok(false)
            }
        }
        false => {
            if !auto_generate {
                return Err(Error::AdminCredentials {
                    message: format!(
                        "Admin credential secret [{}/{}] does not exist.",
                        secret_name, secret_namespace
                    ),
                });
            }
            tracing::info!("No existing admin credentials found, generating a random password.");
            let password: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(15)
                .map(char::from)
                .collect();

            let mut secret_data = BTreeMap::new();

            secret_data.insert(
                SINGLEUSER_USER_KEY.to_string(),
                SINGLEUSER_DEFAULT_ADMIN.to_string(),
            );
            secret_data.insert(SINGLEUSER_PASSWORD_KEY.to_string(), password.to_string());

            let new_secret = Secret {
                metadata: ObjectMetaBuilder::new()
                    .namespace(secret_namespace)
                    .name(secret_name)
                    .build(),
                string_data: Some(secret_data),
                ..Secret::default()
            };
            client
                .create(&new_secret)
                .await
                .with_context(|_| KubeSnafu {
                    reason: format!(
                        "creating new secret for admincredentials: [{}/{}]",
                        secret_name, secret_namespace
                    ),
                })?;
            Ok(true)
        }
    }
}
