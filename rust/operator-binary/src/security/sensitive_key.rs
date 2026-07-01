use std::collections::BTreeMap;

use rand::{RngExt, distr::Alphanumeric};
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder, client::Client, k8s_openapi::api::core::v1::Secret,
    v2::types::kubernetes::NamespaceName,
};

use crate::controller::ValidatedSensitiveProperties;

/// The key under which the generated sensitive-properties key is stored in the Secret. The
/// `nifi.properties` builder references the mounted file by this same name, so the two must agree.
pub const SENSITIVE_PROPERTY_KEY_NAME: &str = "nifiSensitivePropsKey";

/// Mount path of the sensitive-properties key Secret
pub const SENSITIVE_PROPERTY_VOLUME_MOUNT: &str = "/stackable/sensitiveproperty";

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to check sensitive property key secret"))]
    SensitiveKeySecret {
        source: stackable_operator::client::Error,
    },

    #[snafu(display(
        "sensitive key secret [{namespace}/{name}] is missing, but auto generation is disabled",
    ))]
    SensitiveKeySecretMissing { name: String, namespace: String },
}

pub(crate) async fn check_or_generate_sensitive_key(
    client: &Client,
    sensitive_properties: &ValidatedSensitiveProperties,
    namespace: &NamespaceName,
) -> Result<bool, Error> {
    let key_secret = &sensitive_properties.key_secret;
    match client
        .get_opt::<Secret>(key_secret.as_ref(), namespace.as_ref())
        .await
        .context(SensitiveKeySecretSnafu)?
    {
        Some(_) => Ok(false),
        None => {
            if !sensitive_properties.auto_generate {
                return Err(Error::SensitiveKeySecretMissing {
                    name: key_secret.to_string(),
                    namespace: namespace.to_string(),
                });
            }
            tracing::info!("No existing sensitive properties key found, generating new one");
            let password: String = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(15)
                .map(char::from)
                .collect();

            let mut secret_data = BTreeMap::new();
            secret_data.insert(SENSITIVE_PROPERTY_KEY_NAME.to_string(), password);

            let new_secret = Secret {
                metadata: ObjectMetaBuilder::new()
                    .namespace(namespace)
                    .name(key_secret.to_string())
                    .build(),
                string_data: Some(secret_data),
                ..Secret::default()
            };
            client
                .create(&new_secret)
                .await
                .context(SensitiveKeySecretSnafu)?;
            Ok(true)
        }
    }
}
