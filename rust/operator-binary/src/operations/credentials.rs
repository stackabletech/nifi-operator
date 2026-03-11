//! Credential resolution for NiFi REST API authentication.
//!
//! Reads the admin username/password from the Kubernetes Secret referenced
//! by the StaticProvider AuthenticationClass.

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{client::Client, k8s_openapi::api::core::v1::Secret};
use tracing::debug;

use crate::security::authentication::STACKABLE_ADMIN_USERNAME;

/// Errors from NiFi credential resolution.
#[derive(Debug, Snafu)]
pub enum Error {
    /// Failed to fetch the credentials Secret from the Kubernetes API.
    #[snafu(display(
        "failed to get credentials Secret '{secret_name}' in namespace '{namespace}'"
    ))]
    GetSecret {
        source: stackable_operator::client::Error,
        secret_name: String,
        namespace: String,
    },

    /// The credentials Secret does not contain the expected key.
    #[snafu(display("credentials Secret '{secret_name}' is missing the '{key}' key"))]
    MissingSecretKey { secret_name: String, key: String },

    /// The password value in the Secret is not valid UTF-8.
    #[snafu(display("credentials Secret '{secret_name}' key '{key}' contains invalid UTF-8"))]
    InvalidUtf8 {
        source: std::string::FromUtf8Error,
        secret_name: String,
        key: String,
    },
}

/// Resolved admin credentials for NiFi REST API authentication.
pub struct NifiCredentials {
    /// The admin username (always [`STACKABLE_ADMIN_USERNAME`]).
    pub username: String,
    /// The plaintext admin password, with leading/trailing whitespace trimmed.
    pub password: String,
}

/// Read the admin password from the Kubernetes Secret referenced by the
/// StaticProvider AuthenticationClass.
///
/// The Secret format is: key = username, value = plaintext password.
///
/// # Parameters
///
/// - `client`: Kubernetes client with permissions to read Secrets.
/// - `secret_name`: Name of the Kubernetes Secret containing the credentials.
/// - `namespace`: Namespace of the Secret (same as the NiFi cluster).
pub async fn resolve_single_user_credentials(
    client: &Client,
    secret_name: &str,
    namespace: &str,
) -> Result<NifiCredentials, Error> {
    let secret: Secret =
        client
            .get::<Secret>(secret_name, namespace)
            .await
            .context(GetSecretSnafu {
                secret_name: secret_name.to_string(),
                namespace: namespace.to_string(),
            })?;

    let data = secret.data.unwrap_or_default();

    let password_bytes = data
        .get(STACKABLE_ADMIN_USERNAME)
        .context(MissingSecretKeySnafu {
            secret_name: secret_name.to_string(),
            key: STACKABLE_ADMIN_USERNAME.to_string(),
        })?;

    let password = String::from_utf8(password_bytes.0.clone()).context(InvalidUtf8Snafu {
        secret_name: secret_name.to_string(),
        key: STACKABLE_ADMIN_USERNAME.to_string(),
    })?;

    debug!(
        secret_name,
        namespace, "Successfully resolved NiFi admin credentials"
    );

    Ok(NifiCredentials {
        username: STACKABLE_ADMIN_USERNAME.to_string(),
        password: password.trim().to_string(),
    })
}
