use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write,
};

use product_config::{ProductConfigManager, types::PropertyNameKind};
use snafu::{ResultExt, Snafu};
use stackable_operator::product_config_utils::{
    ValidatedRoleConfigByPropertyKind, transform_all_roles_to_config,
    validate_all_roles_and_groups_config,
};
use strum::{Display, EnumIter};

use crate::{
    crd::{NifiRole, NifiRoleType, v1alpha1},
    security::oidc,
};

pub mod jvm;

pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";
pub const NIFI_PYTHON_WORKING_DIRECTORY: &str = "/nifi-python-working-directory";
pub const NIFI_PVC_STORAGE_DIRECTORY: &str = "/stackable/data";

pub const NIFI_BOOTSTRAP_CONF: &str = "bootstrap.conf";
pub const NIFI_PROPERTIES: &str = "nifi.properties";
pub const NIFI_STATE_MANAGEMENT_XML: &str = "state-management.xml";
pub const JVM_SECURITY_PROPERTIES_FILE: &str = "security.properties";

#[derive(Debug, Display, EnumIter)]
pub enum NifiRepository {
    #[strum(serialize = "filebased")]
    Filebased,
    #[strum(serialize = "flowfile")]
    Flowfile,
    #[strum(serialize = "database")]
    Database,
    #[strum(serialize = "content")]
    Content,
    #[strum(serialize = "provenance")]
    Provenance,
    #[strum(serialize = "state")]
    State,
}

impl NifiRepository {
    pub fn repository(&self) -> String {
        format!("{}-repository", self)
    }

    pub fn mount_path(&self) -> String {
        format!("{NIFI_PVC_STORAGE_DIRECTORY}/{}", self)
    }
}

#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("invalid product config"))]
    InvalidProductConfig {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("invalid memory resource configuration - missing default or value in crd?"))]
    MissingMemoryResourceConfig,

    #[snafu(display("invalid JVM config"))]
    InvalidJVMConfig { source: jvm::Error },

    #[snafu(display("failed to transform product configs"))]
    ProductConfigTransform {
        source: stackable_operator::product_config_utils::Error,
    },

    #[snafu(display("failed to calculate storage quota for {repo} repository"))]
    CalculateStorageQuota {
        source: stackable_operator::memory::Error,
        repo: NifiRepository,
    },

    #[snafu(display("failed to generate OIDC config"))]
    GenerateOidcConfig { source: oidc::Error },

    #[snafu(display(
        "NiFi 1.x requires ZooKeeper (hint: upgrade to NiFi 2.x or set .spec.clusterConfig.zookeeperConfigMapName)"
    ))]
    Nifi1RequiresZookeeper,
}

/// Defines all required roles and their required configuration. In this case we need three files:
/// `bootstrap.conf`, `nifi.properties` and `state-management.xml`.
///
/// We do not require any env variables yet. We will however utilize them to change the
/// configuration directory - check <https://github.com/apache/nifi/pull/2985> for more detail.
///
/// The roles and their configs are then validated and complemented by the product config.
///
/// # Arguments
/// * `resource`        - The NifiCluster containing the role definitions.
/// * `version`         - The NifiCluster version.
/// * `product_config`  - The product config to validate and complement the user config.
///
pub fn validated_product_config(
    resource: &v1alpha1::NifiCluster,
    version: &str,
    role: &NifiRoleType,
    product_config: &ProductConfigManager,
) -> Result<ValidatedRoleConfigByPropertyKind, Error> {
    let mut roles = HashMap::new();
    roles.insert(
        NifiRole::Node.to_string(),
        (
            vec![
                PropertyNameKind::File(NIFI_BOOTSTRAP_CONF.to_string()),
                PropertyNameKind::File(NIFI_PROPERTIES.to_string()),
                PropertyNameKind::File(NIFI_STATE_MANAGEMENT_XML.to_string()),
                PropertyNameKind::File(JVM_SECURITY_PROPERTIES_FILE.to_string()),
                PropertyNameKind::Env,
            ],
            role.clone(),
        ),
    );

    let role_config =
        transform_all_roles_to_config(resource, &roles).context(ProductConfigTransformSnafu)?;

    validate_all_roles_and_groups_config(version, &role_config, product_config, false, false)
        .context(InvalidProductConfigSnafu)
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
