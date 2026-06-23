//! The validate step in the NifiCluster controller
//!
//! Synchronously validates inputs that don't require Kubernetes API calls. Produces
//! [`ValidatedCluster`], consumed by the rest of `reconcile_nifi`.

use std::{collections::BTreeMap, str::FromStr as _};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    commons::product_image_selection,
    config::fragment,
    kube::ResourceExt as _,
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
            kubernetes::ConfigMapName,
            operator::{ProductVersion, RoleGroupName},
        },
    },
};
use strum::{EnumDiscriminants, IntoStaticStr};

use super::{
    NifiRoleGroupConfig, ValidatedCluster, ValidatedClusterConfig, ValidatedLogging,
    ValidatedNifiConfig, ValidatedReportingTask, ValidatedRoleConfig, ValidatedSensitiveProperties,
};
use crate::{
    controller::{build::git_sync::build_git_sync_resources, dereference::DereferencedObjects},
    crd::{Container, NifiConfig, NifiRole, sensitive_properties, v1alpha1},
    security::{
        authentication::{self, NifiAuthenticationConfig},
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

    #[snafu(display("object has no nodes defined"))]
    NoNodesDefined,

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

    #[snafu(display("invalid sensitive properties algorithm"))]
    InvalidSensitivePropertiesAlgorithm { source: sensitive_properties::Error },

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

    let sensitive_properties_algorithm = nifi
        .spec
        .cluster_config
        .sensitive_properties
        .algorithm
        .clone()
        .unwrap_or_default();
    sensitive_properties_algorithm
        .check_for_nifi_version(&image.product_version)
        .context(InvalidSensitivePropertiesAlgorithmSnafu)?;

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
    // the `ValidatedCluster` rather than the raw `NifiCluster`. The `nodes` role is mandatory
    // (already enforced by `build_role_group_configs` above), so this is always present.
    let role_config = nifi
        .role_config(&NifiRole::Node)
        .map(|role_config| ValidatedRoleConfig {
            pdb: role_config.common.pod_disruption_budget.clone(),
            listener_class: role_config.listener_class.clone(),
        })
        .context(NoNodesDefinedSnafu)?;

    let namespace = dereferenced_objects.namespace.clone();
    let uid = get_uid(nifi).context(GetUidSnafu)?;

    // `app_version_label_value` is constructed to be a valid label value, so it is always a valid
    // `ProductVersion`. It is used for the `app.kubernetes.io/version` label on built resources.
    let product_version = ProductVersion::from_str(&image.app_version_label_value)
        .expect("the app version label value is a valid product version");

    Ok(ValidatedCluster::new(
        name,
        namespace,
        uid,
        image,
        product_version,
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
            reporting_task: ValidatedReportingTask {
                enabled: nifi.spec.cluster_config.create_reporting_task_job.enabled,
                pod_overrides: nifi
                    .spec
                    .cluster_config
                    .create_reporting_task_job
                    .pod_overrides
                    .clone(),
            },
            host_header_check: nifi.spec.cluster_config.host_header_check.clone(),
        },
    ))
}

pub(crate) fn build_role_group_configs(
    nifi: &v1alpha1::NifiCluster,
    image: &product_image_selection::ResolvedProductImage,
    vector_aggregator_config_map_name: &Option<ConfigMapName>,
) -> Result<BTreeMap<NifiRole, BTreeMap<RoleGroupName, NifiRoleGroupConfig>>> {
    let role = nifi.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;
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
    use stackable_operator::v2::types::kubernetes::ConfigMapName;

    use super::*;

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
