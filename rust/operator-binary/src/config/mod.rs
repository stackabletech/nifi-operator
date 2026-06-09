use std::{collections::BTreeMap, fmt::Write};

use snafu::Snafu;
use strum::{Display, EnumIter};

use crate::security::oidc;

pub mod jvm;

pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";
pub const NIFI_PYTHON_WORKING_DIRECTORY: &str = "/nifi-python-working-directory";
pub const NIFI_PVC_STORAGE_DIRECTORY: &str = "/stackable/data";

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
    #[snafu(display("invalid memory resource configuration - missing default or value in crd?"))]
    MissingMemoryResourceConfig,

    #[snafu(display("invalid JVM config"))]
    InvalidJVMConfig { source: jvm::Error },

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

// TODO: Use crate like https://crates.io/crates/java-properties (currently does not work for Nifi
// because of escapes), to have save handling of escapes etc.
pub(crate) fn format_properties(properties: BTreeMap<String, String>) -> String {
    let mut result = String::new();

    for (key, value) in properties {
        let _ = writeln!(result, "{}={}", key, value);
    }

    result
}
