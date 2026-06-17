//! Builder for `security.properties`.

use std::collections::BTreeMap;

use stackable_operator::v2::config_file_writer::{
    PropertiesWriterError, to_java_properties_string,
};

use crate::controller::ValidatedRoleGroupConfig;

pub fn build(rg: &ValidatedRoleGroupConfig) -> Result<String, PropertiesWriterError> {
    let mut props: BTreeMap<String, String> = BTreeMap::new();
    // Defaults previously injected by deploy/config-spec/properties.yaml:
    props.insert("networkaddress.cache.ttl".to_string(), "30".to_string());
    props.insert(
        "networkaddress.cache.negative.ttl".to_string(),
        "0".to_string(),
    );
    props.extend(rg.config_overrides.security_properties.overrides.clone());
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
        controller::{ValidatedLogging, ValidatedRoleGroupConfig},
        crd::{NifiConfig, v1alpha1::NifiConfigOverrides},
    };

    fn make_rg(overrides: Option<BTreeMap<String, String>>) -> ValidatedRoleGroupConfig {
        use std::str::FromStr as _;

        use stackable_operator::v2::{
            product_logging::framework::ValidatedContainerLogConfigChoice,
            role_utils::JavaCommonConfig, types::operator::RoleGroupName,
        };
        ValidatedRoleGroupConfig {
            name: RoleGroupName::from_str("default").expect("valid role-group name"),
            replicas: Some(1),
            config: NifiConfig::default(),
            config_overrides: NifiConfigOverrides {
                security_properties: KeyValueConfigOverrides {
                    overrides: overrides.unwrap_or_default(),
                },
                ..Default::default()
            },
            env_overrides: EnvVarSet::new(),
            pod_overrides: Default::default(),
            product_specific_common_config: JavaCommonConfig::default(),
            logging: ValidatedLogging {
                nifi_container: ValidatedContainerLogConfigChoice::Automatic(Default::default()),
                vector_container: None,
                enable_vector_agent: false,
            },
            git_sync_resources: Default::default(),
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
