//! Per-file builders for the NiFi rolegroup ConfigMap.
//!
//! Each `<file>` module produces the rendered content for one NiFi config file, serializing
//! `.properties`/`.conf` key/value maps to the Java-properties on-wire format.

use std::{collections::BTreeMap, fmt::Write};

pub mod authorizers;
pub mod bootstrap_conf;
pub mod login_identity_providers;
pub mod nifi_properties;
pub mod product_logging;
pub mod security_properties;
pub mod state_management_xml;

/// Returns a NiFi property value that references the environment variable `name`, resolved by NiFi
/// at runtime (the `${env:...}` reference syntax).
pub(crate) fn env_reference(name: &str) -> String {
    format!("${{env:{name}}}")
}

/// Returns a NiFi property value that references the UTF-8 contents of the file at `path`, resolved
/// by NiFi at runtime (the `${file:...}` reference syntax).
pub(crate) fn file_reference(path: &str) -> String {
    format!("${{file:UTF-8:{path}}}")
}

// TODO: Use crate like https://crates.io/crates/java-properties (currently does not work for Nifi
// because of escapes), to have save handling of escapes etc.
pub(crate) fn format_properties(properties: BTreeMap<String, String>) -> String {
    let mut result = String::new();

    for (key, value) in properties {
        let _ = writeln!(result, "{}={}", key, value);
    }

    result
}

/// The names of the files assembled into the NiFi rolegroup ConfigMap.
#[derive(Clone, Copy, Debug, strum::Display)]
pub enum ConfigFileName {
    #[strum(serialize = "bootstrap.conf")]
    BootstrapConf,
    #[strum(serialize = "nifi.properties")]
    NifiProperties,
    #[strum(serialize = "state-management.xml")]
    StateManagementXml,
    #[strum(serialize = "security.properties")]
    SecurityProperties,
    #[strum(serialize = "login-identity-providers.xml")]
    LoginIdentityProviders,
    #[strum(serialize = "authorizers.xml")]
    Authorizers,
    #[strum(serialize = "logback.xml")]
    Logback,
}

/// Test helpers for constructing a minimal [`ValidatedCluster`] and related types without
/// requiring Kubernetes API access.
///
/// # Design choice — direct construction vs. `validate::validate()`
///
/// NiFi's `validate::validate()` calls `NifiAuthenticationConfig::validate()`, which requires a
/// `DereferencedAuthenticationClasses` value populated with real `AuthenticationClass` objects
/// fetched from the Kubernetes API.  Fabricating those objects in unit tests would require
/// pulling in serialized CRD YAML for operator-rs types that are not part of the nifi-operator
/// crate and would be brittle to maintain.
///
/// Instead, we construct [`ValidatedCluster`] directly from its public fields.  The
/// `NifiAuthenticationConfig::SingleUser` variant contains only an
/// `r#static::v1alpha1::AuthenticationProvider` (a small struct with a single `Secret` name),
/// which we can build without any Kubernetes interaction.  For
/// `ResolvedNifiAuthorizationConfig` and `proxy_hosts` we pick the simplest variants.
///
/// Role-group configs are built via `with_validated_config` on a parsed `NifiCluster`,
/// exactly as the existing `bootstrap_conf` tests do — the YAML fixture is minimal and
/// self-contained.
#[cfg(test)]
pub(crate) mod test_support {
    use std::str::FromStr as _;

    use stackable_operator::{
        commons::product_image_selection::ResolvedProductImage,
        crd::authentication::r#static::v1alpha1::{
            AuthenticationProvider as StaticAuthProvider, UserCredentialsSecretRef,
        },
        kvp::LabelValue,
        v2::types::{
            kubernetes::{NamespaceName, Uid},
            operator::{ClusterName, ProductVersion, RoleGroupName},
        },
    };

    use crate::{
        controller::{
            NifiRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig, ValidatedRoleConfig,
            validate::build_role_group_configs,
        },
        crd::{NifiRole, v1alpha1},
        security::{
            authentication::NifiAuthenticationConfig,
            authorization::ResolvedNifiAuthorizationConfig,
        },
    };

    /// A minimal NiFi cluster YAML.  Mirrors the fixture used by bootstrap_conf tests,
    /// stripped down to the mandatory fields only (NiFi 2.x, Kubernetes clustering backend,
    /// SingleUser auth).
    pub const MINIMAL_NIFI_YAML: &str = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
          namespace: default
        spec:
          image:
            productVersion: 2.9.0
          clusterConfig:
            authentication:
              - authenticationClass: nifi-admin-credentials-simple
            sensitiveProperties:
              keySecret: simple-nifi-sensitive-property-key
              autoGenerate: true
          nodes:
            roleGroups:
              default:
                replicas: 1
    "#;

    /// Build a minimal [`ValidatedCluster`] directly (without Kubernetes API access).
    ///
    /// The cluster uses:
    /// - NiFi 2.9.0 (product version)
    /// - `SingleUser` authentication
    /// - `SingleUser` authorization (no OPA, no file-based)
    /// - `allow_all = true` proxy hosts (i.e. `"*"`)
    /// - Kubernetes clustering backend
    /// - Default `NifiArgon2AesGcm256` sensitive-properties algorithm
    pub fn minimal_validated_cluster() -> ValidatedCluster {
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(MINIMAL_NIFI_YAML).expect("invalid test YAML");

        let image = ResolvedProductImage {
            product_version: "2.9.0".to_string(),
            app_version_label_value: "2.9.0".parse::<LabelValue>().unwrap(),
            image: "oci.stackable.tech/sdp/nifi:2.9.0-stackable0.0.0-dev".to_string(),
            image_pull_policy: "IfNotPresent".to_string(),
            pull_secrets: None,
        };

        let role_group_configs = build_role_group_configs(&nifi, &image, &None)
            .expect("role group configs should merge for minimal fixture");

        let role_config = nifi
            .role_config(&NifiRole::Node)
            .map(|role_config| ValidatedRoleConfig {
                pdb: role_config.common.pod_disruption_budget.clone(),
                listener_class: role_config.listener_class.clone(),
            })
            .expect("the minimal fixture defines the nodes role");

        let name = ClusterName::from_str("simple-nifi").expect("valid cluster name");
        let namespace = NamespaceName::from_str("default").expect("valid namespace");
        let uid = Uid::from_str("e6ac237d-a6d4-43a1-8135-f36506110912").expect("valid uid");
        let product_version = ProductVersion::from_str(&image.app_version_label_value)
            .expect("valid product version");

        ValidatedCluster::new(
            name,
            namespace,
            uid,
            image,
            product_version,
            role_config,
            role_group_configs,
            ValidatedClusterConfig {
                authentication: NifiAuthenticationConfig::SingleUser {
                    provider: StaticAuthProvider {
                        user_credentials_secret: UserCredentialsSecretRef {
                            name: "nifi-admin-credentials-simple".to_string(),
                        },
                    },
                },
                authorization: ResolvedNifiAuthorizationConfig::SingleUser,
                clustering_backend: v1alpha1::NifiClusteringBackend::Kubernetes {},
                sensitive_properties_algorithm: Default::default(), // NifiArgon2AesGcm256
                sensitive_key_secret: nifi
                    .spec
                    .cluster_config
                    .sensitive_properties
                    .key_secret
                    .clone(),
                server_tls_secret_class: nifi.server_tls_secret_class().clone(),
                extra_volumes: nifi.spec.cluster_config.extra_volumes.clone(),
                reporting_task_pod_overrides: nifi
                    .spec
                    .cluster_config
                    .create_reporting_task_job
                    .pod_overrides
                    .clone(),
                host_header_check: nifi.spec.cluster_config.host_header_check.clone(),
            },
        )
    }

    /// Return the "default" role-group config from a [`ValidatedCluster`].
    pub fn default_rg(cluster: &ValidatedCluster) -> &NifiRoleGroupConfig {
        cluster
            .role_group_configs
            .get(&NifiRole::Node)
            .and_then(|rgs| {
                rgs.get(&RoleGroupName::from_str("default").expect("valid role-group name"))
            })
            .expect("minimal_validated_cluster must contain a 'default' role group")
    }
}

#[cfg(test)]
mod tests {
    use super::{env_reference, file_reference};

    #[test]
    fn env_reference_uses_nifi_env_syntax() {
        assert_eq!(env_reference("NODE_ADDRESS"), "${env:NODE_ADDRESS}");
    }

    #[test]
    fn file_reference_uses_nifi_utf8_file_syntax() {
        assert_eq!(
            file_reference("/stackable/sensitiveproperty/nifiSensitivePropsKey"),
            "${file:UTF-8:/stackable/sensitiveproperty/nifiSensitivePropsKey}"
        );
    }
}
