use std::collections::BTreeMap;

use rand::{distributions::Alphanumeric, Rng};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder, client::Client, k8s_openapi::api::core::v1::Secret,
    kube::ResourceExt,
};

use crate::crd::v1alpha1;

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("failed to check sensitive property key secret"))]
    SensitiveKeySecret {
        source: stackable_operator::client::Error,
    },

    #[snafu(display(
        "sensitive key secret [{}/{}] is missing, but auto generation is disabled",
        name,
        namespace
    ))]
    SensitiveKeySecretMissing { name: String, namespace: String },
}

pub(crate) async fn check_or_generate_sensitive_key(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<bool, Error> {
    let sensitive_config = &nifi.spec.cluster_config.sensitive_properties;
    let namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;

    match client
        .get_opt::<Secret>(&sensitive_config.key_secret, namespace)
        .await
        .context(SensitiveKeySecretSnafu)?
    {
        Some(_) => Ok(false),
        None => {
            if !sensitive_config.auto_generate {
                return Err(Error::SensitiveKeySecretMissing {
                    name: sensitive_config.key_secret.clone(),
                    namespace: namespace.to_string(),
                });
            }
            tracing::info!("No existing sensitive properties key found, generating new one");
            let password: String = rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(15)
                .map(char::from)
                .collect();

            let mut secret_data = BTreeMap::new();
            secret_data.insert("nifiSensitivePropsKey".to_string(), password);

            let new_secret = Secret {
                metadata: ObjectMetaBuilder::new()
                    .namespace(namespace)
                    .name(sensitive_config.key_secret.to_string())
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
