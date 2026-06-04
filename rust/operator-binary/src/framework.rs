//! Local additions to `stackable-operator` that are not yet (well) generalized in
//! `stackable_operator::v2::*`.
//!
//! Follow-up: replace these with `stackable_operator::v2::*` imports once upstream
//! reconciles `with_validated_config` (it currently returns a bare `RoleGroup`, and
//! the upstream `RoleGroupConfig` uses `EnvVarSet` rather than a plain map).

pub mod role_utils;
pub mod writer;
