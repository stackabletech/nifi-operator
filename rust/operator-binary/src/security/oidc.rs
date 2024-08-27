use std::collections::{BTreeMap, HashSet};

use rand::{distributions::Alphanumeric, Rng};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::NifiCluster;
use stackable_operator::{
    builder::meta::ObjectMetaBuilder, client::Client, k8s_openapi::api::core::v1::Secret,
    kube::ResourceExt,
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
        "found existing secret [{}/{}], but key {} is missing",
        name,
        namespace,
        STACKABLE_OIDC_ADMIN_PASSWORD_KEY
    ))]
    OidcAdminPasswordKeyMissing { name: String, namespace: String },
}

/// Generate a secret containing the password for the admin user that can access the API. This admin user is the same as for SingleUser authentication.
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
            let keys = secret
                .data
                .unwrap_or_default()
                .into_keys()
                .collect::<HashSet<_>>();
            if keys.contains(STACKABLE_OIDC_ADMIN_PASSWORD_KEY) {
                Ok(false)
            } else {
                OidcAdminPasswordKeyMissingSnafu {
                    name: build_oidc_admin_password_secret_name(nifi),
                    namespace,
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
