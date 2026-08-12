//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedCluster`], consumed by the rest of `reconcile_nifi`.

use std::{collections::BTreeMap, str::FromStr as _};

use snafu::{OptionExt, ResultExt, Snafu, ensure};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    config::fragment,
    k8s_openapi::api::core::v1::Secret,
    kube::{ResourceExt as _, runtime::reflector::ObjectRef},
    product_logging::spec::Logging,
    role_utils::CommonConfiguration,
    v2::{
        builder::pod::container::{EnvVarName, EnvVarSet},
        controller_utils::{self, get_cluster_name, get_uid},
        product_logging::framework::{
            VectorContainerLogConfig, validate_logging_configuration_for_container,
        },
        role_utils::with_validated_config,
        types::{
            kubernetes::{ConfigMapName, NamespaceName},
            operator::{ProductVersion, RoleGroupName},
        },
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use super::{
    NifiRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig, ValidatedLogging,
    ValidatedNifiConfig, ValidatedRoleConfig, ValidatedSensitiveProperties,
};
use crate::{
    controller::{
        build::{
            git_sync::build_git_sync_resources, resource::secret::SENSITIVE_PROPERTY_KEY_NAME,
        },
        dereference::{DereferencedObjects, ExistingSecrets},
    },
    crd::{Container, NifiConfig, NifiRole, sensitive_properties, v1alpha1},
    security::{
        authentication::{self, NifiAuthenticationConfig, STACKABLE_ADMIN_USERNAME},
        authorization::ResolvedNifiAuthorizationConfig,
    },
};

/// The base name of the NiFi product image, used to resolve the fully-qualified image reference.
const CONTAINER_IMAGE_BASE_NAME: &str = "nifi";

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    #[snafu(display("failed to resolve product image"))]
    ResolveProductImage {
        source: product_image_selection::Error,
    },

    #[snafu(display("failed to get the cluster name"))]
    GetClusterName { source: controller_utils::Error },

    #[snafu(display("failed to get the UID"))]
    GetUid { source: controller_utils::Error },

    #[snafu(display("invalid NiFi authentication configuration"))]
    InvalidAuthenticationConfig { source: authentication::Error },

    #[snafu(display("failed to validate the rolegroup config fragment"))]
    ValidateRoleGroupConfig { source: fragment::ValidationError },

    #[snafu(display("the role-group name {role_group:?} is invalid"))]
    ParseRoleGroupName {
        source: stackable_operator::v2::macros::attributed_string_type::Error,
        role_group: String,
    },

    #[snafu(display("environment variable name {name:?} is invalid"))]
    ParseEnvVarName {
        source: stackable_operator::v2::macros::attributed_string_type::Error,
        name: String,
    },

    #[snafu(display("failed to build git-sync resources"))]
    BuildGitSyncResources {
        source: crate::controller::build::git_sync::Error,
    },

    #[snafu(display(
        "the Vector aggregator discovery ConfigMap name is required when the Vector agent is enabled"
    ))]
    MissingVectorAggregatorConfigMapName,

    #[snafu(display("failed to validate logging configuration"))]
    ValidateLoggingConfig {
        source: stackable_operator::v2::product_logging::framework::Error,
    },

    #[snafu(display("the product version {product_version:?} is invalid"))]
    ParseProductVersion {
        source: stackable_operator::v2::macros::attributed_string_type::Error,
        product_version: String,
    },

    #[snafu(display(
        "sensitive key secret [{namespace}/{name}] is missing, but auto generation is disabled",
    ))]
    SensitiveKeySecretMissing { name: String, namespace: String },

    #[snafu(display(
        "the existing sensitive key secret {secret} does not contain the key \
         {SENSITIVE_PROPERTY_KEY_NAME}",
    ))]
    SensitiveKeySecretIncomplete { secret: ObjectRef<Secret> },

    #[snafu(display(
        "the existing admin password secret {secret} does not contain the key \
         {STACKABLE_ADMIN_USERNAME}",
    ))]
    AdminPasswordSecretIncomplete { secret: ObjectRef<Secret> },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Validates the cluster spec and the dereferenced inputs.
pub fn validate(
    nifi: &v1alpha1::NifiCluster,
    dereferenced_objects: &DereferencedObjects,
    operator_environment: &OperatorEnvironmentOptions,
) -> Result<ValidatedCluster> {
    let image = nifi
        .spec
        .image
        .resolve(
            CONTAINER_IMAGE_BASE_NAME,
            &operator_environment.image_repository,
            crate::built_info::PKG_VERSION,
        )
        .context(ResolveProductImageSnafu)?;

    let name = get_cluster_name(nifi).context(GetClusterNameSnafu)?;

    let authentication_config =
        NifiAuthenticationConfig::validate(&name, &dereferenced_objects.authentication_classes)
            .context(InvalidAuthenticationConfigSnafu)?;

    let authorization_config = ResolvedNifiAuthorizationConfig::validate(
        &nifi.spec.cluster_config.authorization,
        &dereferenced_objects.authorization,
    );

    validate_existing_secrets(
        &dereferenced_objects.existing_secrets,
        &nifi.spec.cluster_config.sensitive_properties,
        &authentication_config,
        &dereferenced_objects.namespace,
    )?;

    let sensitive_properties_algorithm = nifi
        .spec
        .cluster_config
        .sensitive_properties
        .algorithm
        .clone()
        .unwrap_or_default();

    // The Vector aggregator discovery ConfigMap name is validated by the CRD's typed field. It is
    // only required when the Vector agent is enabled for a role group.
    let vector_aggregator_config_map_name = nifi
        .spec
        .cluster_config
        .vector_aggregator_config_map_name
        .clone();

    let role_group_configs =
        build_role_group_configs(nifi, &image, &vector_aggregator_config_map_name)?;

    // Per-role config (PDB + listener class), extracted here so downstream builders source it from
    // the `ValidatedCluster` rather than the raw `NifiCluster`. The `nodes` role is required by
    // the CRD, so this is always present.
    let node_role_config = nifi.role_config(&NifiRole::Node);
    let role_config = ValidatedRoleConfig {
        pdb: node_role_config.common.pod_disruption_budget.clone(),
        listener_class: node_role_config.listener_class.clone(),
    };

    let namespace = dereferenced_objects.namespace.clone();
    let cluster_domain = dereferenced_objects.cluster_domain.clone();
    let uid = get_uid(nifi).context(GetUidSnafu)?;

    // `app_version_label_value` is constructed to be a valid label value, so it is always a valid
    // `ProductVersion`. It is used for the `app.kubernetes.io/version` label on built resources.
    let product_version = ProductVersion::from_str(&image.app_version_label_value)
        .expect("the app version label value is a valid product version");

    // The bare product version, reported as `status.deployedVersion`. Unlike
    // `app_version_label_value` this is the user's input copied verbatim (it is never truncated to
    // the label value length limit), so it has to be parsed fallibly.
    let deployed_product_version =
        ProductVersion::from_str(&image.product_version).with_context(|_| {
            ParseProductVersionSnafu {
                product_version: image.product_version.clone(),
            }
        })?;

    Ok(ValidatedCluster::new(
        name,
        namespace,
        cluster_domain,
        uid,
        image,
        product_version,
        deployed_product_version,
        role_config,
        role_group_configs,
        ValidatedClusterConfig {
            authentication: authentication_config,
            authorization: authorization_config,
            clustering_backend: nifi.spec.cluster_config.clustering_backend.clone(),
            sensitive_properties: ValidatedSensitiveProperties {
                algorithm: sensitive_properties_algorithm,
                key_secret: nifi
                    .spec
                    .cluster_config
                    .sensitive_properties
                    .key_secret
                    .clone(),
                auto_generate: nifi.spec.cluster_config.sensitive_properties.auto_generate,
            },
            server_tls_secret_class: nifi.server_tls_secret_class().clone(),
            extra_volumes: nifi.spec.cluster_config.extra_volumes.clone(),
            host_header_check: nifi.spec.cluster_config.host_header_check.clone(),
        },
        dereferenced_objects.existing_secrets.clone(),
    ))
}

/// Checks the preconditions on the Secrets whose contents this operator generates.
///
/// The build step only ever *creates* a missing Secret, it never rewrites an existing one (see the
/// [`secret`](crate::controller::build::resource::secret) module docs). So a Secret that is present
/// but does not carry its expected key is rejected here, rather than surfacing later as NiFi Pods
/// that cannot start.
///
/// Rejecting rather than filling in the missing key is deliberate: a regenerated
/// sensitive-properties key cannot decrypt the sensitive values of an already persisted flow, so
/// silently writing a fresh one would destroy data. The admin password Secret follows the same rule
/// for consistency; it is not this operator's to overwrite either.
fn validate_existing_secrets(
    existing_secrets: &ExistingSecrets,
    sensitive_properties: &sensitive_properties::NifiSensitivePropertiesConfig,
    authentication: &NifiAuthenticationConfig,
    namespace: &NamespaceName,
) -> Result<()> {
    match &existing_secrets.sensitive_key {
        Some(secret) => ensure!(
            secret_contains_key(secret, SENSITIVE_PROPERTY_KEY_NAME),
            SensitiveKeySecretIncompleteSnafu {
                secret: ObjectRef::from_obj(secret),
            }
        ),
        // Without `autoGenerate` the Secret is provided and owned by the user, so this operator
        // never creates it and it is merely required to exist.
        None => ensure!(
            sensitive_properties.auto_generate,
            SensitiveKeySecretMissingSnafu {
                name: sensitive_properties.key_secret.to_string(),
                namespace: namespace.to_string(),
            }
        ),
    }

    // The admin password Secret is only mounted for OIDC authentication; a leftover from a
    // previous authentication method must not fail the cluster. It is named by this operator, so
    // unlike the sensitive key Secret it is never required to exist up-front.
    if matches!(authentication, NifiAuthenticationConfig::Oidc { .. })
        && let Some(secret) = &existing_secrets.oidc_admin_password
    {
        ensure!(
            secret_contains_key(secret, STACKABLE_ADMIN_USERNAME),
            AdminPasswordSecretIncompleteSnafu {
                secret: ObjectRef::from_obj(secret),
            }
        );
    }

    Ok(())
}

/// Whether the given fetched Secret carries `key`. Only `data` is inspected: the API server always
/// returns the contents there, `string_data` is write-only.
fn secret_contains_key(secret: &Secret, key: &str) -> bool {
    secret
        .data
        .iter()
        .flat_map(BTreeMap::keys)
        .any(|existing| existing == key)
}

pub(crate) fn build_role_group_configs(
    nifi: &v1alpha1::NifiCluster,
    image: &product_image_selection::ResolvedProductImage,
    vector_aggregator_config_map_name: &Option<ConfigMapName>,
) -> Result<BTreeMap<NifiRole, BTreeMap<RoleGroupName, NifiRoleGroupConfig>>> {
    let role = &nifi.spec.nodes;
    let default_config = NifiConfig::default_config(&nifi.name_any(), &NifiRole::Node);

    let mut groups: BTreeMap<RoleGroupName, NifiRoleGroupConfig> = BTreeMap::new();
    for (rg_name, rg) in &role.role_groups {
        let role_group_name =
            RoleGroupName::from_str(rg_name).with_context(|_| ParseRoleGroupNameSnafu {
                role_group: rg_name.clone(),
            })?;
        let validated = with_validated_config::<NifiConfig, _, _, _, _>(rg, role, &default_config)
            .context(ValidateRoleGroupConfigSnafu)?;

        let CommonConfiguration {
            config,
            config_overrides,
            env_overrides,
            cli_overrides,
            pod_overrides,
            product_specific_common_config,
        } = validated.config;

        // Convert the merged env-override HashMap into an EnvVarSet, validating each name
        // eagerly. Keys are unique (HashMap), so insertion order is irrelevant.
        let mut env_overrides_set = EnvVarSet::new();
        for (name, value) in env_overrides {
            env_overrides_set = env_overrides_set.with_value(
                &EnvVarName::from_str(&name)
                    .context(ParseEnvVarNameSnafu { name: name.clone() })?,
                value,
            );
        }

        // Validate the logging config (NiFi + optional Vector container) up-front so an invalid
        // custom log ConfigMap name, or a missing Vector aggregator discovery ConfigMap name, fails
        // during validation rather than at resource-build time.
        let logging = validate_logging(&config.logging, vector_aggregator_config_map_name)?;

        // The git-sync resources depend on this role group's env-var overrides and logging config,
        // so they are resolved (and validated) per role group up-front rather than at build time.
        let git_sync_resources = build_git_sync_resources(
            &nifi.spec.cluster_config.custom_components_git_sync,
            image,
            &config,
            &env_overrides_set,
        )
        .context(BuildGitSyncResourcesSnafu)?;

        groups.insert(
            role_group_name,
            NifiRoleGroupConfig {
                replicas: validated.replicas,
                config: ValidatedNifiConfig::from_merged(config, logging, git_sync_resources),
                config_overrides,
                env_overrides: env_overrides_set,
                cli_overrides,
                pod_overrides,
                product_specific_common_config,
            },
        );
    }

    let mut role_group_configs = BTreeMap::new();
    role_group_configs.insert(NifiRole::Node, groups);
    Ok(role_group_configs)
}

/// Validates the logging configuration for the NiFi (and optional Vector) container.
///
/// `vector_aggregator_config_map_name` is the discovery ConfigMap name of the Vector aggregator;
/// it is required (and validated) only when the Vector agent is enabled.
fn validate_logging(
    logging: &Logging<Container>,
    vector_aggregator_config_map_name: &Option<ConfigMapName>,
) -> Result<ValidatedLogging> {
    let nifi_container = validate_logging_configuration_for_container(logging, &Container::Nifi)
        .context(ValidateLoggingConfigSnafu)?;

    let prepare_container =
        validate_logging_configuration_for_container(logging, &Container::Prepare)
            .context(ValidateLoggingConfigSnafu)?;

    let vector_container = if logging.enable_vector_agent {
        let vector_aggregator_config_map_name = vector_aggregator_config_map_name
            .clone()
            .context(MissingVectorAggregatorConfigMapNameSnafu)?;
        Some(VectorContainerLogConfig {
            log_config: validate_logging_configuration_for_container(logging, &Container::Vector)
                .context(ValidateLoggingConfigSnafu)?,
            vector_aggregator_config_map_name,
        })
    } else {
        None
    };

    Ok(ValidatedLogging {
        nifi_container,
        prepare_container,
        vector_container,
        enable_vector_agent: logging.enable_vector_agent,
    })
}

/// A minimal resolved product image (NiFi 2.9.0) for tests that need to build role-group configs.
#[cfg(test)]
pub(crate) fn test_resolved_product_image() -> product_image_selection::ResolvedProductImage {
    product_image_selection::ResolvedProductImage {
        product_version: "2.9.0".to_string(),
        app_version_label_value: "2.9.0".parse().expect("valid label value"),
        image: "oci.stackable.tech/sdp/nifi:2.9.0-stackable0.0.0-dev".to_string(),
        image_pull_policy: "IfNotPresent".to_string(),
        pull_secrets: None,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use stackable_operator::{
        commons::networking::DomainName,
        crd::authentication::core as auth_core,
        k8s_openapi::{ByteString, apimachinery::pkg::apis::meta::v1::ObjectMeta},
        v2::types::{kubernetes::ConfigMapName, operator::ClusterName},
    };

    use super::*;
    use crate::{
        controller::{
            build::properties::test_support::{app_version_label, oidc_authentication_config},
            dereference::ExistingSecrets,
        },
        security::{
            authentication::DereferencedAuthenticationClasses,
            authorization::DereferencedAuthorization,
        },
    };

    /// The name of the sensitive-properties key Secret in every fixture below.
    const SENSITIVE_KEY_SECRET_NAME: &str = "simple-nifi-sensitive-property-key";

    /// `spec.clusterConfig.sensitiveProperties` with the given `autoGenerate` setting.
    fn sensitive_properties(
        auto_generate: bool,
    ) -> sensitive_properties::NifiSensitivePropertiesConfig {
        serde_yaml::from_str(&format!(
            "keySecret: {SENSITIVE_KEY_SECRET_NAME}\nautoGenerate: {auto_generate}"
        ))
        .expect("valid sensitiveProperties")
    }

    /// A Secret as the API server returns it: contents in `data`, under the given `keys`. Only the
    /// key names matter here, so the values are a fixed placeholder.
    fn fetched_secret(name: &str, keys: &[&str]) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                namespace: Some("default".to_owned()),
                ..ObjectMeta::default()
            },
            data: Some(
                keys.iter()
                    .map(|key| ((*key).to_owned(), ByteString(b"irrelevant".to_vec())))
                    .collect(),
            ),
            ..Secret::default()
        }
    }

    fn single_user_authentication() -> NifiAuthenticationConfig {
        NifiAuthenticationConfig::SingleUser {
            provider: serde_yaml::from_str(
                "userCredentialsSecret:\n  name: nifi-admin-credentials-simple",
            )
            .expect("valid static provider"),
        }
    }

    fn test_namespace() -> NamespaceName {
        NamespaceName::from_str("default").expect("valid namespace")
    }

    /// With `autoGenerate` the build step creates the Secret, so it may be absent.
    #[test]
    fn accepts_a_missing_sensitive_key_secret_when_auto_generation_is_enabled() {
        let existing_secrets = ExistingSecrets {
            sensitive_key: None,
            oidc_admin_password: None,
        };

        validate_existing_secrets(
            &existing_secrets,
            &sensitive_properties(true),
            &single_user_authentication(),
            &test_namespace(),
        )
        .expect("a missing Secret is generated by the build step");
    }

    /// Without `autoGenerate` the Secret is the user's to provide, so it has to be there already.
    #[test]
    fn rejects_a_missing_sensitive_key_secret_when_auto_generation_is_disabled() {
        let existing_secrets = ExistingSecrets {
            sensitive_key: None,
            oidc_admin_password: None,
        };

        let error = validate_existing_secrets(
            &existing_secrets,
            &sensitive_properties(false),
            &single_user_authentication(),
            &test_namespace(),
        )
        .expect_err("the missing Secret must be reported");

        assert!(
            matches!(error, Error::SensitiveKeySecretMissing { .. }),
            "unexpected error: {error:?}"
        );
    }

    /// An existing Secret is never rewritten, so one without the expected key would leave the NiFi
    /// Pods with an unusable mount — regardless of `autoGenerate`.
    #[test]
    fn rejects_an_existing_sensitive_key_secret_without_its_key() {
        for auto_generate in [true, false] {
            let existing_secrets = ExistingSecrets {
                sensitive_key: Some(fetched_secret(
                    SENSITIVE_KEY_SECRET_NAME,
                    &["some-other-key"],
                )),
                oidc_admin_password: None,
            };

            let error = validate_existing_secrets(
                &existing_secrets,
                &sensitive_properties(auto_generate),
                &single_user_authentication(),
                &test_namespace(),
            )
            .expect_err("the incomplete Secret must be reported");

            assert!(
                matches!(error, Error::SensitiveKeySecretIncomplete { .. }),
                "unexpected error for autoGenerate={auto_generate}: {error:?}"
            );
        }
    }

    #[test]
    fn rejects_an_existing_admin_password_secret_without_its_key() {
        let cluster_name = ClusterName::from_str("simple-nifi").expect("valid cluster name");
        let existing_secrets = ExistingSecrets {
            sensitive_key: Some(fetched_secret(
                SENSITIVE_KEY_SECRET_NAME,
                &[SENSITIVE_PROPERTY_KEY_NAME],
            )),
            oidc_admin_password: Some(fetched_secret(
                "simple-nifi-oidc-admin-password",
                &["some-other-user"],
            )),
        };

        let error = validate_existing_secrets(
            &existing_secrets,
            &sensitive_properties(true),
            &oidc_authentication_config(&cluster_name),
            &test_namespace(),
        )
        .expect_err("the incomplete Secret must be reported");

        assert!(
            matches!(error, Error::AdminPasswordSecretIncomplete { .. }),
            "unexpected error: {error:?}"
        );
    }

    /// The admin password Secret is only mounted for OIDC, so a leftover from a previous
    /// authentication method must not fail the cluster.
    #[test]
    fn ignores_the_admin_password_secret_for_other_authentication_methods() {
        let existing_secrets = ExistingSecrets {
            sensitive_key: Some(fetched_secret(
                SENSITIVE_KEY_SECRET_NAME,
                &[SENSITIVE_PROPERTY_KEY_NAME],
            )),
            oidc_admin_password: Some(fetched_secret(
                "simple-nifi-oidc-admin-password",
                &["some-other-user"],
            )),
        };

        validate_existing_secrets(
            &existing_secrets,
            &sensitive_properties(true),
            &single_user_authentication(),
            &test_namespace(),
        )
        .expect("the unused Secret must be ignored");
    }

    /// Locks every value the validate step itself derives from the minimal fixture — so a
    /// validation regression fails here, with a validate-shaped message, instead of surfacing as
    /// a confusing build-test failure downstream.
    ///
    /// The merged per-role-group config (resources, affinity, logging defaults, …) is produced by
    /// `with_validated_config` and the config defaults, whose contracts are tested in operator-rs;
    /// only the values this module derives on top are re-asserted here.
    #[test]
    fn validate_ok_derives_expected_values() {
        let yaml = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
          namespace: default
          uid: e6ac237d-a6d4-43a1-8135-f36506110912
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
        let nifi: v1alpha1::NifiCluster = serde_yaml::from_str(yaml).expect("valid test YAML");

        let auth_class: auth_core::v1alpha1::AuthenticationClass = serde_yaml::from_str(
            r#"
            metadata:
              name: nifi-admin-credentials-simple
            spec:
              provider: !static
                userCredentialsSecret:
                  name: nifi-admin-credentials-simple
            "#,
        )
        .expect("valid static AuthenticationClass");
        let auth_entry: auth_core::v1alpha1::ClientAuthenticationDetails =
            serde_yaml::from_str("authenticationClass: nifi-admin-credentials-simple")
                .expect("valid authentication entry");

        let dereferenced_objects = DereferencedObjects {
            namespace: "default".parse().expect("valid namespace"),
            cluster_domain: DomainName::from_str("cluster.local").expect("valid cluster domain"),
            authentication_classes: DereferencedAuthenticationClasses::from_entries(vec![(
                auth_entry, auth_class,
            )]),
            authorization: DereferencedAuthorization::without_opa(),
            // As on the first reconcile run: neither Secret exists yet.
            existing_secrets: ExistingSecrets {
                sensitive_key: None,
                oidc_admin_password: None,
            },
        };
        let operator_environment = OperatorEnvironmentOptions {
            operator_namespace: "stackable-operators".to_owned(),
            operator_service_name: "nifi-operator".to_owned(),
            image_repository: "oci.example.org".to_owned(),
        };

        let cluster = validate(&nifi, &dereferenced_objects, &operator_environment)
            .expect("the minimal fixture validates");

        assert_eq!(cluster.name.to_string(), "simple-nifi");
        assert_eq!(cluster.namespace.to_string(), "default");
        assert_eq!(
            cluster.uid.to_string(),
            "e6ac237d-a6d4-43a1-8135-f36506110912"
        );
        assert_eq!(cluster.cluster_domain.to_string(), "cluster.local");
        assert_eq!(
            cluster.image.image,
            format!("oci.example.org/nifi:{}", app_version_label("2.9.0"))
        );
        assert_eq!(cluster.image.product_version, "2.9.0");
        // The label value carries the `-stackable<operator version>` suffix, the version reported
        // in `status.deployedVersion` does not.
        assert_eq!(
            cluster.product_version.to_string(),
            app_version_label("2.9.0")
        );
        assert_eq!(cluster.deployed_product_version.to_string(), "2.9.0");

        // The role config falls back to its defaults: PDBs enabled, cluster-internal listener.
        assert!(cluster.role_config.pdb.enabled);
        assert_eq!(cluster.role_config.pdb.max_unavailable, None);
        assert_eq!(
            cluster.role_config.listener_class.to_string(),
            "cluster-internal"
        );

        // SingleUser authentication and authorization, the default (Kubernetes) clustering
        // backend, and the default `tls` server SecretClass.
        let cluster_config = &cluster.cluster_config;
        assert!(matches!(
            &cluster_config.authentication,
            NifiAuthenticationConfig::SingleUser { provider }
                if provider.user_credentials_secret.name == "nifi-admin-credentials-simple"
        ));
        assert!(matches!(
            cluster_config.authorization,
            ResolvedNifiAuthorizationConfig::SingleUser
        ));
        assert!(matches!(
            cluster_config.clustering_backend,
            v1alpha1::NifiClusteringBackend::Kubernetes {}
        ));
        assert_eq!(cluster_config.server_tls_secret_class.to_string(), "tls");
        assert_eq!(
            cluster_config.sensitive_properties.key_secret.to_string(),
            "simple-nifi-sensitive-property-key"
        );
        assert!(cluster_config.sensitive_properties.auto_generate);
        assert!(cluster_config.extra_volumes.is_empty());

        // A single `node` role with the single `default` role group; the Vector agent is off.
        assert_eq!(cluster.role_group_configs.len(), 1);
        let role_groups = &cluster.role_group_configs[&NifiRole::Node];
        let role_group_names: Vec<String> = role_groups.keys().map(ToString::to_string).collect();
        assert_eq!(role_group_names, ["default"]);
        let role_group = role_groups
            .values()
            .next()
            .expect("the default role group exists");
        assert_eq!(role_group.replicas, Some(1));
        assert!(!role_group.config.logging.enable_vector_agent);
        assert_eq!(role_group.config.logging.vector_container, None);
    }

    /// A NiFi cluster with the Vector agent enabled at the Node role level.
    const NIFI_VECTOR_ENABLED_YAML: &str = r#"
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
            config:
              logging:
                enableVectorAgent: true
            roleGroups:
              default:
                replicas: 1
    "#;

    /// A minimal NiFi cluster with the Vector agent disabled (the default).
    const NIFI_VECTOR_DISABLED_YAML: &str = r#"
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

    fn default_rg(
        configs: &BTreeMap<NifiRole, BTreeMap<RoleGroupName, NifiRoleGroupConfig>>,
    ) -> &NifiRoleGroupConfig {
        configs[&NifiRole::Node]
            .get(&RoleGroupName::from_str("default").expect("valid role-group name"))
            .expect("the 'default' role group must exist")
    }

    #[test]
    fn vector_container_is_validated_when_agent_enabled() {
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(NIFI_VECTOR_ENABLED_YAML).expect("invalid test YAML");
        let aggregator = Some(ConfigMapName::from_str("nifi-vector-aggregator-discovery").unwrap());

        let configs = build_role_group_configs(&nifi, &test_resolved_product_image(), &aggregator)
            .expect("role group configs should validate");

        let vector = default_rg(&configs)
            .config
            .logging
            .vector_container
            .as_ref()
            .expect("the Vector container config should be present when the agent is enabled");
        assert_eq!(
            "nifi-vector-aggregator-discovery",
            vector.vector_aggregator_config_map_name.to_string()
        );
    }

    #[test]
    fn vector_agent_enabled_without_aggregator_name_fails() {
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(NIFI_VECTOR_ENABLED_YAML).expect("invalid test YAML");

        // `NifiRoleGroupConfig` is not `Debug` (its `config` holds non-`Debug` git-sync resources),
        // so match on the result rather than using `expect_err` (which would require `Ok` to be
        // `Debug`).
        let result = build_role_group_configs(&nifi, &test_resolved_product_image(), &None);
        assert!(matches!(
            result,
            Err(Error::MissingVectorAggregatorConfigMapName)
        ));
    }

    #[test]
    fn no_vector_container_when_agent_disabled() {
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(NIFI_VECTOR_DISABLED_YAML).expect("invalid test YAML");

        // The aggregator name is not required when the Vector agent is disabled.
        let configs = build_role_group_configs(&nifi, &test_resolved_product_image(), &None)
            .expect("role group configs should validate");

        assert!(
            default_rg(&configs)
                .config
                .logging
                .vector_container
                .is_none()
        );
    }
}
