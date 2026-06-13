//! Builders that assemble Kubernetes resources from a [`ValidatedCluster`].
//!
//! [`ValidatedCluster`]: crate::controller::ValidatedCluster

use snafu::Snafu;

use crate::{crd::storage::NifiRepository, security::oidc};

pub mod config_map;
pub mod git_sync;
pub mod graceful_shutdown;
pub mod jvm;
pub mod properties;
pub mod proxy_hosts;
pub mod resource;

/// Errors that can occur while building the NiFi product configuration files.
#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
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
