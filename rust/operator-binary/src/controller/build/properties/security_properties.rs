//! Builder for `security.properties`.

use std::collections::BTreeMap;

use stackable_operator::v2::config_file_writer::{
    PropertiesWriterError, to_java_properties_string,
};

use super::ConfigFileName;
use crate::controller::validate::NifiRoleGroupConfig;

pub fn build(rg: &NifiRoleGroupConfig) -> Result<String, PropertiesWriterError> {
    let mut props: BTreeMap<String, String> = BTreeMap::new();
    // Defaults previously injected by deploy/config-spec/properties.yaml:
    props.insert("networkaddress.cache.ttl".to_string(), "30".to_string());
    props.insert(
        "networkaddress.cache.negative.ttl".to_string(),
        "0".to_string(),
    );
    for (k, v) in super::resolved_overrides_for(rg, ConfigFileName::SecurityProperties) {
        props.insert(k, v);
    }
    to_java_properties_string(props.iter())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use stackable_operator::v2::{
        builder::pod::container::EnvVarSet, config_overrides::KeyValueConfigOverrides,
    };

    use super::*;
    use crate::{
        controller::validate::NifiRoleGroupConfig,
        crd::{NifiConfig, v1alpha1::NifiConfigOverrides},
    };

    fn make_rg(overrides: Option<BTreeMap<String, String>>) -> NifiRoleGroupConfig {
        use stackable_operator::role_utils::JavaCommonConfig;
        NifiRoleGroupConfig {
            replicas: 1,
            config: NifiConfig::default(),
            config_overrides: NifiConfigOverrides {
                security_properties: KeyValueConfigOverrides {
                    overrides: overrides
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(k, v)| (k, Some(v)))
                        .collect(),
                },
                ..Default::default()
            },
            env_overrides: EnvVarSet::new(),
            cli_overrides: BTreeMap::new(),
            pod_overrides: Default::default(),
            product_specific_common_config: JavaCommonConfig::default(),
        }
    }

    #[test]
    fn test_default_keys_present() {
        let rg = make_rg(None);
        let result = build(&rg).unwrap();
        assert!(result.contains("networkaddress.cache.ttl=30"));
        assert!(result.contains("networkaddress.cache.negative.ttl=0"));
    }

    #[test]
    fn test_user_override_wins() {
        let mut overrides = BTreeMap::new();
        overrides.insert("networkaddress.cache.ttl".to_string(), "60".to_string());
        let rg = make_rg(Some(overrides));
        let result = build(&rg).unwrap();
        assert!(result.contains("networkaddress.cache.ttl=60"));
        assert!(!result.contains("networkaddress.cache.ttl=30"));
    }
}
