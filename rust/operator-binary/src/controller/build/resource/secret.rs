//! Builds the Secrets whose contents this operator generates: the sensitive-properties key and,
//! for OIDC authentication, the admin password.
//!
//! Both are emitted only while they do not exist yet, as determined by the dereference step. Their
//! contents are randomly generated, so an identical Secret can never be *rebuilt*, and rewriting an
//! existing one would rotate contents that have to stay stable — a fresh sensitive-properties key
//! cannot decrypt the sensitive values of the persisted flow.
//!
//! Emitting them only once is enough because, unlike the operator-generated Secrets of the sibling
//! operators, these are deliberately not owned by the NifiCluster (see [`secret_meta`]) and hence
//! never orphan-deleted. So there is nothing to re-emit them for.
//!
//! Everything that would leave an existing Secret unusable is rejected by the validate step.

use std::collections::BTreeMap;

use rand::{RngExt, distr::Alphanumeric};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    k8s_openapi::{api::core::v1::Secret, apimachinery::pkg::apis::meta::v1::ObjectMeta},
    v2::types::operator::ClusterName,
};

use crate::{
    controller::ValidatedCluster,
    security::authentication::{NifiAuthenticationConfig, STACKABLE_ADMIN_USERNAME},
};

/// The key under which the sensitive-properties key is stored in its Secret. The `nifi.properties`
/// builder references the mounted file by this same name, so the two must agree.
pub const SENSITIVE_PROPERTY_KEY_NAME: &str = "nifiSensitivePropsKey";

/// The length of the passwords generated here.
const GENERATED_PASSWORD_LENGTH: usize = 15;

/// Builds every Secret of this cluster: the sensitive-properties key and, for OIDC
/// authentication, the admin password.
///
/// Infallible: everything that could make a Secret unusable is rejected by the validate step,
/// which checks that an existing Secret carries its expected key and that a missing one may be
/// generated.
pub fn build_secrets(cluster: &ValidatedCluster) -> Vec<Secret> {
    build_sensitive_key_secret(cluster)
        .into_iter()
        .chain(build_oidc_admin_password_secret(cluster))
        .collect()
}

/// The Secret holding the key with which NiFi encrypts the sensitive properties of its
/// processors, mounted by the NiFi Pods.
///
/// Only emitted when `autoGenerate` is set and the Secret does not exist yet. Without
/// `autoGenerate` the Secret is provided and owned by the user, so this operator must not write to
/// it at all. The validate step merely requires it to exist.
fn build_sensitive_key_secret(cluster: &ValidatedCluster) -> Option<Secret> {
    let sensitive_properties = &cluster.cluster_config.sensitive_properties;
    if !sensitive_properties.auto_generate || cluster.existing_secrets.sensitive_key.is_some() {
        return None;
    }

    let name = sensitive_properties.key_secret.to_string();
    tracing::info!(
        secret.name = name,
        "No existing sensitive properties key found, generating new one"
    );
    Some(generate_secret(cluster, &name, SENSITIVE_PROPERTY_KEY_NAME))
}

/// The name of the Secret built by [`build_oidc_admin_password_secret`], which the StatefulSet
/// builder mounts and the dereference step looks up.
pub fn build_oidc_admin_password_secret_name(cluster_name: &ClusterName) -> String {
    format!("{cluster_name}-oidc-admin-password")
}

/// The Secret holding the password of the admin user that can access the API, mounted by the NiFi
/// Pods. This admin user is the same as for SingleUser authentication.
///
/// Only emitted for OIDC authentication — the only authentication method that uses it — and only
/// while the Secret does not exist yet.
fn build_oidc_admin_password_secret(cluster: &ValidatedCluster) -> Option<Secret> {
    if !matches!(
        cluster.cluster_config.authentication,
        NifiAuthenticationConfig::Oidc { .. }
    ) || cluster.existing_secrets.oidc_admin_password.is_some()
    {
        return None;
    }

    let name = build_oidc_admin_password_secret_name(&cluster.name);
    tracing::info!(
        secret.name = name,
        "No existing oidc admin password secret found, generating new one"
    );
    Some(generate_secret(cluster, &name, STACKABLE_ADMIN_USERNAME))
}

/// A Secret holding a freshly generated random password under the given `key`.
fn generate_secret(cluster: &ValidatedCluster, name: &str, key: &str) -> Secret {
    let password: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(GENERATED_PASSWORD_LENGTH)
        .map(char::from)
        .collect();

    Secret {
        metadata: secret_meta(cluster, name),
        string_data: Some(BTreeMap::from([(key.to_string(), password)])),
        ..Secret::default()
    }
}

/// Metadata of a generated Secret.
///
/// Deliberately carries no owner reference, unlike every other resource built by this operator:
/// both Secrets have to outlive the NifiCluster. The sensitive-properties key still decrypts the
/// persisted flow after the cluster is recreated, and it may even have been created by the user
/// rather than by this operator. Not being owned by the cluster also keeps them out of
/// `ClusterResources`' orphan listing, which only considers directly owned resources.
fn secret_meta(cluster: &ValidatedCluster, name: &str) -> ObjectMeta {
    ObjectMetaBuilder::new()
        .name_and_namespace(cluster)
        .name(name)
        .with_labels(cluster.cluster_shared_recommended_labels())
        .build()
}

#[cfg(test)]
mod tests {
    use stackable_operator::kube::ResourceExt as _;

    use super::*;
    use crate::controller::build::properties::test_support::{
        app_version_label, minimal_validated_cluster, oidc_authentication_config,
    };

    /// A Secret as it comes back from the API server: contents in `data`, plus the server-owned
    /// metadata that must not be echoed into an apply patch.
    fn fetched_secret(name: &str, key: &str) -> Secret {
        serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": name,
                "namespace": "default",
                "resourceVersion": "12345",
                "uid": "0a3b1f0e-1111-2222-3333-444455556666",
            },
            "data": { key: "b2xkLXNlY3JldA==" },
        }))
        .expect("valid Secret")
    }

    fn oidc_cluster() -> ValidatedCluster {
        let mut cluster = minimal_validated_cluster();
        let authentication = oidc_authentication_config(&cluster.name);
        cluster.cluster_config.authentication = authentication;
        cluster
    }

    #[test]
    fn generates_the_sensitive_key_secret_when_it_is_missing() {
        let secrets = build_secrets(&minimal_validated_cluster());

        let [secret] = secrets.as_slice() else {
            panic!("SingleUser authentication only needs the sensitive key Secret");
        };
        assert_eq!(secret.name_any(), "simple-nifi-sensitive-property-key");
        assert_eq!(
            secret
                .string_data
                .as_ref()
                .expect("a generated Secret carries its contents in string_data")
                .get(SENSITIVE_PROPERTY_KEY_NAME)
                .map(String::len),
            Some(GENERATED_PASSWORD_LENGTH)
        );
    }

    /// An existing Secret is left alone: it is not owned by the cluster, so nothing deletes it,
    /// and rewriting it would rotate a key that has to keep decrypting the persisted flow.
    #[test]
    fn does_not_emit_an_existing_sensitive_key_secret() {
        let mut cluster = minimal_validated_cluster();
        cluster.existing_secrets.sensitive_key = Some(fetched_secret(
            "simple-nifi-sensitive-property-key",
            SENSITIVE_PROPERTY_KEY_NAME,
        ));

        let secrets = build_secrets(&cluster);

        assert!(secrets.is_empty());
    }

    /// Without `autoGenerate` the Secret belongs to the user, so it is only required to exist and
    /// is never emitted (and hence never written to).
    #[test]
    fn never_emits_a_user_provided_sensitive_key_secret() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.sensitive_properties.auto_generate = false;
        cluster.existing_secrets.sensitive_key = Some(fetched_secret(
            "simple-nifi-sensitive-property-key",
            SENSITIVE_PROPERTY_KEY_NAME,
        ));

        let secrets = build_secrets(&cluster);

        assert!(secrets.is_empty());
    }

    #[test]
    fn generates_the_oidc_admin_password_secret_when_it_is_missing() {
        let secrets = build_secrets(&oidc_cluster());

        let [_sensitive_key, admin_password] = secrets.as_slice() else {
            panic!("OIDC authentication needs both Secrets");
        };
        assert_eq!(admin_password.name_any(), "simple-nifi-oidc-admin-password");
        assert!(
            admin_password
                .string_data
                .as_ref()
                .expect("a generated Secret carries its contents in string_data")
                .contains_key(STACKABLE_ADMIN_USERNAME)
        );
    }

    #[test]
    fn does_not_emit_an_existing_oidc_admin_password_secret() {
        let mut cluster = oidc_cluster();
        cluster.existing_secrets.oidc_admin_password = Some(fetched_secret(
            "simple-nifi-oidc-admin-password",
            STACKABLE_ADMIN_USERNAME,
        ));

        let secrets = build_secrets(&cluster);

        let [sensitive_key] = secrets.as_slice() else {
            panic!("only the still missing sensitive key Secret may be emitted");
        };
        assert_eq!(
            sensitive_key.name_any(),
            "simple-nifi-sensitive-property-key"
        );
    }

    #[test]
    fn omits_the_oidc_admin_password_secret_for_other_authentication_methods() {
        // The minimal fixture uses SingleUser authentication.
        let secrets = build_secrets(&minimal_validated_cluster());

        assert!(
            !secrets
                .iter()
                .any(|secret| secret.name_any() == "simple-nifi-oidc-admin-password")
        );
    }

    /// Locks the metadata both Secrets carry: the labels `ClusterResources::add` requires (without
    /// them the apply step rejects the resource) and the deliberately absent owner reference.
    ///
    /// [`ClusterResources::add`]: stackable_operator::cluster_resources::ClusterResources::add
    #[test]
    fn secret_metadata_is_labelled_but_not_owned_by_the_cluster() {
        let secrets = build_secrets(&oidc_cluster());

        for secret in &secrets {
            assert_eq!(
                serde_json::to_value(&secret.metadata).expect("must be serializable"),
                serde_json::json!({
                    // The Secrets are cluster-shared, so role and role group are `none`.
                    "labels": {
                        "app.kubernetes.io/component": "none",
                        "app.kubernetes.io/instance": "simple-nifi",
                        "app.kubernetes.io/managed-by": "nifi.stackable.tech_nificluster",
                        "app.kubernetes.io/name": "nifi",
                        "app.kubernetes.io/role-group": "none",
                        "app.kubernetes.io/version": app_version_label("2.9.0"),
                        "stackable.tech/vendor": "Stackable"
                    },
                    "name": secret.name_any(),
                    "namespace": "default",
                }),
            );
        }
    }
}
