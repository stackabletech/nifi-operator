//! Per-file builders for the NiFi rolegroup ConfigMap.
//!
//! Each `<file>` module produces the rendered content for one NiFi config file.
//! The shared [`writer`] module serializes `.properties`/`.conf` key/value maps to
//! the Java-properties on-wire format.

use std::collections::BTreeMap;

use stackable_operator::config_overrides::KeyValueConfigOverrides;

pub mod writer;

/// The names of the files assembled into the NiFi rolegroup ConfigMap.
#[allow(dead_code)] // used once the per-file builders land in Task 4
#[derive(Clone, Copy, Debug, strum::Display)]
pub enum ConfigFileName {
    #[strum(serialize = "bootstrap.conf")]
    BootstrapConf,
    #[strum(serialize = "nifi.properties")]
    NifiProperties,
    #[strum(serialize = "state-management.xml")]
    StateManagementXml,
    #[strum(serialize = "security.properties")]
    SecurityProperties,
    #[strum(serialize = "login-identity-providers.xml")]
    LoginIdentityProviders,
    #[strum(serialize = "authorizers.xml")]
    Authorizers,
}

/// Keep only the set (`Some`) entries of a `key -> optional value` map, as `(key, value)` pairs.
#[allow(dead_code)] // used once the per-file builders land in Task 4
fn defined_entries(
    entries: BTreeMap<String, Option<String>>,
) -> impl Iterator<Item = (String, String)> {
    entries
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key, value)))
}

/// Resolve user-provided [`KeyValueConfigOverrides`] into key/value pairs.
#[allow(dead_code)] // used once the per-file builders land in Task 4
fn resolved_overrides(
    overrides: KeyValueConfigOverrides,
) -> impl Iterator<Item = (String, String)> {
    overrides.overrides.into_iter()
}
