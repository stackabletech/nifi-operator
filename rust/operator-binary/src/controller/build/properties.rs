//! Per-file builders for the NiFi rolegroup ConfigMap.
//!
//! Each `<file>` module produces the rendered content for one NiFi config file.
//! The shared [`writer`] module serializes `.properties`/`.conf` key/value maps to
//! the Java-properties on-wire format.

use std::collections::BTreeMap;

use stackable_operator::config_overrides::KeyValueOverridesProvider;

use crate::controller::validate::NifiRoleGroupConfig;

pub mod authorizers;
pub mod bootstrap_conf;
pub mod login_identity_providers;
pub mod nifi_properties;
pub mod security_properties;
pub mod state_management_xml;
pub mod writer;

/// The names of the files assembled into the NiFi rolegroup ConfigMap.
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
fn defined_entries(
    entries: BTreeMap<String, Option<String>>,
) -> impl Iterator<Item = (String, String)> {
    entries
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key, value)))
}

/// Resolve the user overrides for `file` from a rolegroup's config overrides, dropping unset values.
pub(crate) fn resolved_overrides_for(
    rg: &NifiRoleGroupConfig,
    file: ConfigFileName,
) -> impl Iterator<Item = (String, String)> {
    defined_entries(
        rg.config_overrides
            .get_key_value_overrides(&file.to_string()),
    )
}
