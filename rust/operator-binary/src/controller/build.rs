//! Builders that assemble Kubernetes resources from a [`ValidatedCluster`].
//!
//! [`ValidatedCluster`]: crate::controller::validate::ValidatedCluster

pub mod config_map;
pub mod git_sync;
pub mod properties;
pub mod proxy_hosts;
