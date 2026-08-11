//! Builds the Secrets whose contents this operator generates: the sensitive-properties key and,
//! for OIDC authentication, the admin password.
//!
//! Their contents are randomly generated, so an identical Secret can never be *rebuilt*. Instead
//! the contents fetched in the dereference step are re-emitted unchanged, which makes applying
//! them a no-op. That way each Secret is emitted on *every* reconcile run and can be applied and
//! tracked like every other resource, rather than being a read-or-create side effect outside the
//! regular pipeline.

use std::collections::BTreeMap;

use rand::{RngExt, distr::Alphanumeric};
use snafu::{Snafu, ensure};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    k8s_openapi::{api::core::v1::Secret, apimachinery::pkg::apis::meta::v1::ObjectMeta},
    kube::runtime::reflector::ObjectRef,
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

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display(
        "sensitive key secret [{namespace}/{name}] is missing, but auto generation is disabled",
    ))]
    SensitiveKeySecretMissing { name: String, namespace: String },

    #[snafu(display(
        "found existing admin password secret {secret}, but the key {STACKABLE_ADMIN_USERNAME} is missing",
    ))]
    MissingAdminPasswordKey { secret: ObjectRef<Secret> },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Builds every Secret of this cluster: the sensitive-properties key and, for OIDC
/// authentication, the admin password.
pub fn build_secrets(cluster: &ValidatedCluster) -> Result<Vec<Secret>> {
    Ok(build_sensitive_key_secret(cluster)?
        .into_iter()
        .chain(build_oidc_admin_password_secret(cluster)?)
        .collect())
}

/// The Secret holding the key with which NiFi encrypts the sensitive properties of its
/// processors, mounted by the NiFi Pods.
///
/// Only emitted when `autoGenerate` is set. Without it the Secret is provided and owned by the
/// user, so this operator must not write to it at all — it is merely required to exist.
///
/// A Secret that is already present is re-emitted unchanged (see the module docs); regenerating
/// it would render the sensitive properties of the persisted flow undecryptable.
fn build_sensitive_key_secret(cluster: &ValidatedCluster) -> Result<Option<Secret>> {
    let sensitive_properties = &cluster.cluster_config.sensitive_properties;
    let name = sensitive_properties.key_secret.to_string();
    let existing = cluster.existing_secrets.sensitive_key.as_ref();

    if !sensitive_properties.auto_generate {
        ensure!(
            existing.is_some(),
            SensitiveKeySecretMissingSnafu {
                name,
                namespace: cluster.namespace.to_string(),
            }
        );
        return Ok(None);
    }

    Ok(Some(match existing {
        Some(existing) => reemit_secret(cluster, &name, existing),
        None => {
            tracing::info!(
                secret.name = name,
                "No existing sensitive properties key found, generating new one"
            );
            generate_secret(cluster, &name, SENSITIVE_PROPERTY_KEY_NAME)
        }
    }))
}

/// The name of the Secret built by [`build_oidc_admin_password_secret`], which the StatefulSet
/// builder mounts and the dereference step looks up.
pub fn build_oidc_admin_password_secret_name(cluster_name: &ClusterName) -> String {
    format!("{cluster_name}-oidc-admin-password")
}

/// The Secret holding the password of the admin user that can access the API, mounted by the NiFi
/// Pods. This admin user is the same as for SingleUser authentication.
///
/// Only emitted for OIDC authentication, which is the only authentication method that uses it.
fn build_oidc_admin_password_secret(cluster: &ValidatedCluster) -> Result<Option<Secret>> {
    if !matches!(
        cluster.cluster_config.authentication,
        NifiAuthenticationConfig::Oidc { .. }
    ) {
        return Ok(None);
    }

    let name = build_oidc_admin_password_secret_name(&cluster.name);

    Ok(Some(match &cluster.existing_secrets.oidc_admin_password {
        Some(existing) => {
            // An existing Secret without the admin password is not replaced: it was not
            // created by this operator, so overwriting it would clobber whatever it holds.
            let admin_password_present = existing
                .data
                .iter()
                .flat_map(|data| data.keys())
                .any(|key| key == STACKABLE_ADMIN_USERNAME);
            ensure!(
                admin_password_present,
                MissingAdminPasswordKeySnafu {
                    secret: ObjectRef::from_obj(existing),
                }
            );

            reemit_secret(cluster, &name, existing)
        }
        None => {
            tracing::info!(
                secret.name = name,
                "No existing oidc admin password secret found, generating new one"
            );
            generate_secret(cluster, &name, STACKABLE_ADMIN_USERNAME)
        }
    }))
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

/// Re-emits an existing Secret, carrying its fetched `data` over unchanged: the contents are
/// randomly generated at creation and cannot be rebuilt, so echoing them back is the only way to
/// emit the Secret on every run without rotating its contents. Applying identical contents
/// changes nothing on the server (no watch event, no propagation into the Pods, no restart).
///
/// The metadata is built fresh rather than echoed, because a fetched object carries
/// server-populated fields (`resourceVersion`, `uid`, `managedFields`) that must not appear in an
/// apply patch.
fn reemit_secret(cluster: &ValidatedCluster, name: &str, existing: &Secret) -> Secret {
    Secret {
        metadata: secret_meta(cluster, name),
        data: existing.data.clone(),
        ..Secret::default()
    }
}

/// Metadata shared by the freshly generated and the re-emitted Secret, so that the two are
/// identical apart from their contents.
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
    use stackable_operator::{
        commons::tls_verification::TlsClientDetails, crd::authentication::oidc,
        k8s_openapi::ByteString, kube::ResourceExt as _,
    };

    use super::*;
    use crate::controller::build::properties::test_support::{
        app_version_label, minimal_validated_cluster,
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
        let cluster_name = cluster.name.clone();
        cluster.cluster_config.authentication = NifiAuthenticationConfig::Oidc {
            provider: oidc::v1alpha1::AuthenticationProvider::new(
                "keycloak.mycorp.org".to_owned().try_into().unwrap(),
                Some(443),
                "/realms/sdp".to_owned(),
                TlsClientDetails { tls: None },
                "preferred_username".to_owned(),
                vec!["openid".to_owned()],
                None,
            ),
            oidc: oidc::v1alpha1::ClientAuthenticationOptions {
                client_credentials_secret_ref: "nifi-keycloak-client".to_owned(),
                extra_scopes: vec![],
                product_specific_fields: (),
            },
            cluster_name,
        };
        cluster
    }

    #[test]
    fn generates_the_sensitive_key_secret_when_it_is_missing() {
        let secrets =
            build_secrets(&minimal_validated_cluster()).expect("the Secrets must be buildable");

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

    /// An existing Secret is re-emitted with its fetched contents unchanged, so that applying it
    /// is a no-op instead of rotating the key on every reconcile run.
    #[test]
    fn reemits_the_existing_sensitive_key_secret_unchanged() {
        let mut cluster = minimal_validated_cluster();
        cluster.existing_secrets.sensitive_key = Some(fetched_secret(
            "simple-nifi-sensitive-property-key",
            SENSITIVE_PROPERTY_KEY_NAME,
        ));

        let secrets = build_secrets(&cluster).expect("the Secrets must be buildable");

        let [secret] = secrets.as_slice() else {
            panic!("SingleUser authentication only needs the sensitive key Secret");
        };
        assert_eq!(
            secret.data.as_ref().and_then(|data| data
                .get(SENSITIVE_PROPERTY_KEY_NAME)
                .map(|ByteString(value)| value.clone())),
            Some(b"old-secret".to_vec()),
            "the fetched contents must be carried over unchanged"
        );
        assert!(
            secret.string_data.is_none(),
            "nothing may be regenerated for an existing Secret"
        );
        // The server-populated metadata of the fetched Secret must not end up in the apply patch.
        assert_eq!(secret.metadata.resource_version, None);
        assert_eq!(secret.metadata.uid, None);
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

        let secrets = build_secrets(&cluster).expect("the Secrets must be buildable");

        assert!(secrets.is_empty());
    }

    #[test]
    fn fails_when_the_sensitive_key_secret_is_missing_and_auto_generation_is_disabled() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.sensitive_properties.auto_generate = false;

        let error = build_secrets(&cluster).expect_err("the missing Secret must be reported");

        assert!(
            matches!(error, Error::SensitiveKeySecretMissing { .. }),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn generates_the_oidc_admin_password_secret_when_it_is_missing() {
        let secrets = build_secrets(&oidc_cluster()).expect("the Secrets must be buildable");

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
    fn reemits_the_existing_oidc_admin_password_secret_unchanged() {
        let mut cluster = oidc_cluster();
        cluster.existing_secrets.oidc_admin_password = Some(fetched_secret(
            "simple-nifi-oidc-admin-password",
            STACKABLE_ADMIN_USERNAME,
        ));

        let secrets = build_secrets(&cluster).expect("the Secrets must be buildable");

        let [_sensitive_key, admin_password] = secrets.as_slice() else {
            panic!("OIDC authentication needs both Secrets");
        };
        assert_eq!(
            admin_password.data,
            fetched_secret("simple-nifi-oidc-admin-password", STACKABLE_ADMIN_USERNAME).data,
            "the fetched contents must be carried over unchanged"
        );
        assert!(
            admin_password.string_data.is_none(),
            "nothing may be regenerated for an existing Secret"
        );
    }

    #[test]
    fn fails_when_the_existing_oidc_admin_password_secret_has_no_admin_password() {
        let mut cluster = oidc_cluster();
        cluster.existing_secrets.oidc_admin_password = Some(fetched_secret(
            "simple-nifi-oidc-admin-password",
            "some-other-user",
        ));

        let error = build_secrets(&cluster).expect_err("the incomplete Secret must be reported");

        assert!(
            matches!(error, Error::MissingAdminPasswordKey { .. }),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn omits_the_oidc_admin_password_secret_for_other_authentication_methods() {
        // The minimal fixture uses SingleUser authentication.
        let secrets =
            build_secrets(&minimal_validated_cluster()).expect("the Secrets must be buildable");

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
        let secrets = build_secrets(&oidc_cluster()).expect("the Secrets must be buildable");

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
