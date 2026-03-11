//! Product-specific operations for the NiFi operator.
//!
//! - [`credentials`]: Resolves NiFi admin credentials from Kubernetes Secrets.
//! - [`nifi_api`]: REST API client for NiFi cluster management.
//! - [`scaling`]: Scaling hooks for NiFi node decommissioning.

pub mod credentials;
pub mod graceful_shutdown;
pub mod nifi_api;
pub mod pdb;
pub mod scaling;
pub mod upgrade;
