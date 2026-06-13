//! Builds the git-sync resources (volumes, mounts, containers) for a NiFi Node rolegroup.

use snafu::{ResultExt, Snafu};
use stackable_operator::{crd::git_sync, k8s_openapi::api::core::v1::EnvVar};

use crate::{
    controller::{
        ValidatedCluster, ValidatedRoleGroupConfig, build::resource::statefulset::LOG_VOLUME_NAME,
    },
    crd::Container,
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("invalid git-sync specification"))]
    InvalidGitSyncSpec { source: git_sync::v1alpha2::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Builds the [`git_sync::v1alpha2::GitSyncResources`] for a single Node rolegroup. The env vars
/// and logging configuration differ per rolegroup, so the resources are computed per rolegroup
/// rather than once for the whole cluster.
pub fn build_git_sync_resources(
    cluster: &ValidatedCluster,
    rg: &ValidatedRoleGroupConfig,
) -> Result<git_sync::v1alpha2::GitSyncResources> {
    let env_vars: Vec<EnvVar> = rg.env_overrides.clone().into();
    git_sync::v1alpha2::GitSyncResources::new(
        &cluster.cluster_config.custom_components_git_sync,
        &cluster.image,
        &env_vars,
        &[],
        LOG_VOLUME_NAME,
        &rg.config.logging.for_container(&Container::GitSync),
    )
    .context(InvalidGitSyncSpecSnafu)
}
