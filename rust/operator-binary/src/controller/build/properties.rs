//! Per-file builders for the NiFi rolegroup ConfigMap.
//!
//! Each `<file>` module produces the rendered content for one NiFi config file.
//! The shared [`stackable_operator::v2::config_file_writer`] module serializes `.properties`/`.conf`
//! key/value maps to the Java-properties on-wire format.

use crate::controller::validate::NifiRoleGroupConfig;

pub mod authorizers;
pub mod bootstrap_conf;
pub mod logging;
pub mod login_identity_providers;
pub mod nifi_properties;
pub mod security_properties;
pub mod state_management_xml;

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

/// Resolve the user overrides for `file` from a rolegroup's config overrides.
///
/// Keys whose value is unset (`None`, i.e. `key: null` in YAML) are dropped, so only the defined
/// `(key, value)` pairs reach the property writer.
pub(crate) fn resolved_overrides_for(
    rg: &NifiRoleGroupConfig,
    file: ConfigFileName,
) -> impl Iterator<Item = (String, String)> + '_ {
    let overrides = match file {
        ConfigFileName::BootstrapConf => Some(&rg.config_overrides.bootstrap_conf),
        ConfigFileName::NifiProperties => Some(&rg.config_overrides.nifi_properties),
        ConfigFileName::SecurityProperties => Some(&rg.config_overrides.security_properties),
        ConfigFileName::StateManagementXml
        | ConfigFileName::LoginIdentityProviders
        | ConfigFileName::Authorizers
        | ConfigFileName::Logback => None,
    };
    overrides
        .into_iter()
        .flat_map(|o| &o.overrides)
        .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
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
/// crate and would be fragile to upstream changes.
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
    use std::{collections::BTreeMap, str::FromStr as _};

    use stackable_operator::{
        commons::product_image_selection::ResolvedProductImage,
        crd::authentication::r#static::v1alpha1::{
            AuthenticationProvider as StaticAuthProvider, UserCredentialsSecretRef,
        },
        kube::ResourceExt as _,
        kvp::LabelValue,
        v2::types::{
            kubernetes::{NamespaceName, Uid},
            operator::ClusterName,
        },
    };

    use crate::{
        controller::validate::{NifiRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig},
        crd::{NifiConfig, NifiRole, v1alpha1},
        framework::role_utils::with_validated_config,
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

        let nifi_role = NifiRole::Node;
        let role = nifi.spec.nodes.as_ref().unwrap();
        let default_config = NifiConfig::default_config(&nifi.name_any(), &nifi_role);

        let mut role_groups: BTreeMap<String, NifiRoleGroupConfig> = BTreeMap::new();
        for (rg_name, rg) in &role.role_groups {
            let validated_rg =
                with_validated_config::<NifiConfig, _, _, _, _>(rg, role, &default_config)
                    .expect("with_validated_config should succeed for minimal fixture");
            role_groups.insert(rg_name.clone(), validated_rg);
        }
        let mut role_group_configs = BTreeMap::new();
        role_group_configs.insert(NifiRole::Node, role_groups);

        let image = ResolvedProductImage {
            product_version: "2.9.0".to_string(),
            app_version_label_value: "2.9.0".parse::<LabelValue>().unwrap(),
            image: "oci.stackable.tech/sdp/nifi:2.9.0-stackable0.0.0-dev".to_string(),
            image_pull_policy: "IfNotPresent".to_string(),
            pull_secrets: None,
        };

        let name = ClusterName::from_str("simple-nifi").expect("valid cluster name");
        let namespace = NamespaceName::from_str("default").expect("valid namespace");
        let uid = Uid::from_str("e6ac237d-a6d4-43a1-8135-f36506110912").expect("valid uid");

        ValidatedCluster::new(
            name,
            namespace,
            uid,
            image,
            nifi.spec.nodes.clone().expect("minimal fixture has nodes"),
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
                host_header_check: nifi.spec.cluster_config.host_header_check.clone(),
                custom_components_git_sync: nifi
                    .spec
                    .cluster_config
                    .custom_components_git_sync
                    .clone(),
            },
        )
    }

    /// Return the "default" role-group config from a [`ValidatedCluster`].
    pub fn default_rg(cluster: &ValidatedCluster) -> &NifiRoleGroupConfig {
        cluster
            .role_group_configs
            .get(&NifiRole::Node)
            .and_then(|rgs| rgs.get("default"))
            .expect("minimal_validated_cluster must contain a 'default' role group")
    }

    /// Build an empty [`GitSyncResources`] (no git-sync configured).
    pub fn empty_git_sync_resources()
    -> stackable_operator::crd::git_sync::v1alpha2::GitSyncResources {
        stackable_operator::crd::git_sync::v1alpha2::GitSyncResources::default()
    }
}
