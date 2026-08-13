//! The security-related inputs of a NifiCluster: authentication, authorization and TLS.
//!
//! These modules resolve and validate what the spec asks for; the Kubernetes resources derived
//! from them are assembled by [`crate::controller::build`].

pub mod authentication;
pub mod authorization;
pub mod tls;
