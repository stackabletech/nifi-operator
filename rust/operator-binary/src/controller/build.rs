//! Builders that assemble Kubernetes resources from a [`ValidatedCluster`].
//!
//! [`ValidatedCluster`]: crate::controller::ValidatedCluster

use std::str::FromStr;

use snafu::Snafu;
use stackable_operator::v2::types::operator::RoleGroupName;

use crate::{crd::storage::NifiRepository, security::oidc};

pub mod config_map;
pub mod git_sync;
pub mod graceful_shutdown;
pub mod jvm;
pub mod properties;
pub mod proxy_hosts;
pub mod resource;

// Placeholder role-group name for role-level resources (e.g. the per-role `Listener`), which have
// no associated role group. Preserves the historical `app.kubernetes.io/role-group: none` label.
stackable_operator::constant!(pub(crate) PLACEHOLDER_LISTENER_ROLE_GROUP: RoleGroupName = "none");

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
