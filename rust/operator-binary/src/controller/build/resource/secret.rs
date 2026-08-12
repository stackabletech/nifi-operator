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

    /// The shared [`minimal_validated_cluster`] fixture, with the properties these tests depend on
    /// spelled out and asserted: a cluster that asks for exactly one generated Secret, the
    /// sensitive-properties key.
    fn minimal_cluster() -> ValidatedCluster {
        let cluster = minimal_validated_cluster();
        assert!(
            cluster.cluster_config.sensitive_properties.auto_generate,
            "fixture precondition: sensitiveProperties.autoGenerate is on"
        );
        assert!(
            !matches!(
                cluster.cluster_config.authentication,
                NifiAuthenticationConfig::Oidc { .. }
            ),
            "fixture precondition: authentication is not OIDC, so no admin password is needed"
        );
        assert!(
            cluster.existing_secrets.sensitive_key.is_none()
                && cluster.existing_secrets.oidc_admin_password.is_none(),
            "fixture precondition: as on the first reconcile run, neither Secret exists yet"
        );
        cluster
    }

    /// The same fixture switched over to OIDC authentication, which additionally needs the admin
    /// password Secret.
    fn oidc_cluster() -> ValidatedCluster {
        let mut cluster = minimal_cluster();
        let authentication = oidc_authentication_config(&cluster.name);
        cluster.cluster_config.authentication = authentication;
        cluster
    }

    /// The Secret name the cluster asks for in `spec.clusterConfig.sensitiveProperties.keySecret`,
    /// read off the fixture so the assertions below cannot drift from it.
    fn sensitive_key_secret_name(cluster: &ValidatedCluster) -> String {
        cluster
            .cluster_config
            .sensitive_properties
            .key_secret
            .to_string()
    }

    /// An existing Secret, as the dereference step hands it over. Only its presence matters to the
    /// build step, whether its contents are usable is the validate step's business — so it
    /// deliberately carries none.
    fn existing_secret(name: &str) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                namespace: Some("default".to_owned()),
                ..ObjectMeta::default()
            },
            ..Secret::default()
        }
    }

    fn names(secrets: &[Secret]) -> Vec<String> {
        secrets.iter().map(|secret| secret.name_any()).collect()
    }

    /// Looks a Secret up by name: the order in which [`build_secrets`] returns them is not part of
    /// its contract.
    fn secret_named<'a>(secrets: &'a [Secret], name: &str) -> &'a Secret {
        secrets
            .iter()
            .find(|secret| secret.name_any() == name)
            .unwrap_or_else(|| panic!("no Secret named {name:?} among {:?}", names(secrets)))
    }

    #[test]
    fn generates_the_sensitive_key_secret_when_it_is_missing() {
        let cluster = minimal_cluster();

        let secrets = build_secrets(&cluster);

        assert_eq!(names(&secrets), [sensitive_key_secret_name(&cluster)]);
        assert_eq!(
            secrets[0]
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
        let mut cluster = minimal_cluster();
        let name = sensitive_key_secret_name(&cluster);
        cluster.existing_secrets.sensitive_key = Some(existing_secret(&name));

        let secrets = build_secrets(&cluster);

        assert_eq!(names(&secrets), [] as [String; 0]);
    }

    /// Without `autoGenerate` the Secret belongs to the user, so this operator never writes it —
    /// not even while it is missing, a case the validate step rejects before the build step runs.
    #[test]
    fn never_generates_a_user_provided_sensitive_key_secret() {
        let mut cluster = minimal_cluster();
        cluster.cluster_config.sensitive_properties.auto_generate = false;

        let secrets = build_secrets(&cluster);

        assert_eq!(names(&secrets), [] as [String; 0]);
    }

    #[test]
    fn generates_the_oidc_admin_password_secret_when_it_is_missing() {
        let cluster = oidc_cluster();
        let name = build_oidc_admin_password_secret_name(&cluster.name);

        let secrets = build_secrets(&cluster);

        assert!(
            secret_named(&secrets, &name)
                .string_data
                .as_ref()
                .expect("a generated Secret carries its contents in string_data")
                .contains_key(STACKABLE_ADMIN_USERNAME)
        );
    }

    #[test]
    fn does_not_emit_an_existing_oidc_admin_password_secret() {
        let mut cluster = oidc_cluster();
        let name = build_oidc_admin_password_secret_name(&cluster.name);
        cluster.existing_secrets.oidc_admin_password = Some(existing_secret(&name));

        let secrets = build_secrets(&cluster);

        assert_eq!(
            names(&secrets),
            [sensitive_key_secret_name(&cluster)],
            "only the still missing sensitive key Secret may be emitted"
        );
    }

    #[test]
    fn omits_the_oidc_admin_password_secret_for_other_authentication_methods() {
        // `minimal_cluster` asserts that the fixture does not use OIDC authentication.
        let cluster = minimal_cluster();

        let secrets = build_secrets(&cluster);

        assert!(!names(&secrets).contains(&build_oidc_admin_password_secret_name(&cluster.name)));
    }

    /// Locks the metadata both Secrets carry: the labels `ClusterResources::add` requires (without
    /// them the apply step rejects the resource) and the deliberately absent owner reference.
    ///
    /// [`ClusterResources::add`]: stackable_operator::cluster_resources::ClusterResources::add
    #[test]
    fn secret_metadata_is_labelled_but_not_owned_by_the_cluster() {
        let cluster = oidc_cluster();

        let secrets = build_secrets(&cluster);

        let mut emitted = names(&secrets);
        emitted.sort();
        assert_eq!(
            emitted,
            [
                build_oidc_admin_password_secret_name(&cluster.name),
                sensitive_key_secret_name(&cluster),
            ],
            "both Secrets must be checked, not an empty list"
        );
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
