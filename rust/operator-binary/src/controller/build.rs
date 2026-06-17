//! Builders that assemble Kubernetes resources from a [`ValidatedCluster`].
//!
//! [`ValidatedCluster`]: crate::controller::ValidatedCluster

use std::str::FromStr;

use stackable_operator::v2::types::{common::Port, operator::RoleGroupName};

pub mod git_sync;
pub mod graceful_shutdown;
pub mod jvm;
pub mod properties;
pub mod proxy_hosts;
pub mod resource;

// Placeholder role-group name for role-level resources (e.g. the per-role `Listener`), which have
// no associated role group. Preserves the historical `app.kubernetes.io/role-group: none` label.
stackable_operator::constant!(pub(crate) PLACEHOLDER_LISTENER_ROLE_GROUP: RoleGroupName = "none");

pub const HTTPS_PORT_NAME: &str = "https";
pub const HTTPS_PORT: Port = Port(8443);
pub const PROTOCOL_PORT_NAME: &str = "protocol";
pub const PROTOCOL_PORT: Port = Port(9088);
pub const BALANCE_PORT_NAME: &str = "balance";
pub const BALANCE_PORT: Port = Port(6243);
pub const METRICS_PORT_NAME: &str = "metrics";
pub const METRICS_PORT: Port = Port(8081);

// Filesystem paths shared by multiple builders. Single-consumer paths live in their builder.
pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";
pub const NIFI_PYTHON_WORKING_DIRECTORY: &str = "/nifi-python-working-directory";
