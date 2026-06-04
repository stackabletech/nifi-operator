//! Builder for `security.properties`.

use std::collections::BTreeMap;

use super::ConfigFileName;
use crate::{controller::validate::NifiRoleGroupConfig, framework::writer};

pub fn build(rg: &NifiRoleGroupConfig) -> Result<String, writer::PropertiesWriterError> {
    let mut props: BTreeMap<String, Option<String>> = BTreeMap::new();
    // Defaults previously injected by deploy/config-spec/properties.yaml:
    props.insert(
        "networkaddress.cache.ttl".to_string(),
        Some("30".to_string()),
    );
    props.insert(
        "networkaddress.cache.negative.ttl".to_string(),
        Some("0".to_string()),
    );
    for (k, v) in super::resolved_overrides_for(rg, ConfigFileName::SecurityProperties) {
        props.insert(k, Some(v));
    }
    writer::to_java_properties_string(props.iter())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use stackable_operator::config_overrides::KeyValueConfigOverrides;

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
                security_properties: overrides.map(|o| KeyValueConfigOverrides { overrides: o }),
                ..Default::default()
            },
            env_overrides: BTreeMap::new(),
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
