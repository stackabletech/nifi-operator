use stackable_operator::{
    commons::{
        affinity::{StackableAffinityFragment, affinity_between_role_pods},
        opa::OpaConfig,
    },
    k8s_openapi::api::core::v1::{PodAffinity, PodAntiAffinity},
};

use crate::crd::{APP_NAME, NifiRole};

pub fn get_affinity(
    cluster_name: &str,
    role: &NifiRole,
    opa_config: Option<&OpaConfig>,
) -> StackableAffinityFragment {
    // With OPA authorization configured, NiFi sends its authorization requests to OPA, so prefer
    // to place it next to the OPA Pods.
    let pod_affinity = opa_config.map(|opa_config| PodAffinity {
        preferred_during_scheduling_ignored_during_execution: Some(vec![
            affinity_between_role_pods(
                "opa",
                &opa_config.config_map_name, // The discovery cm has the same name as the OpaCluster itself
                "server",
                50,
            ),
        ]),
        required_during_scheduling_ignored_during_execution: None,
    });

    StackableAffinityFragment {
        pod_affinity,
        pod_anti_affinity: Some(PodAntiAffinity {
            preferred_during_scheduling_ignored_during_execution: Some(vec![
                affinity_between_role_pods(APP_NAME, cluster_name, &role.to_string(), 70),
            ]),
            required_during_scheduling_ignored_during_execution: None,
        }),
        node_affinity: None,
        node_selector: None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use stackable_operator::{
        commons::affinity::StackableAffinity,
        k8s_openapi::{
            api::core::v1::{
                PodAffinity, PodAffinityTerm, PodAntiAffinity, WeightedPodAffinityTerm,
            },
            apimachinery::pkg::apis::meta::v1::LabelSelector,
        },
        v2::types::operator::RoleGroupName,
    };

    use super::*;
    use crate::{
        controller::validate::{build_role_group_configs, test_resolved_product_image},
        crd::v1alpha1,
    };

    #[test]
    fn test_affinity_defaults() {
        let input = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
        spec:
          image:
            productVersion: 2.9.0
          clusterConfig:
            authentication:
              - authenticationClass: nifi-admin-credentials-simple
            sensitiveProperties:
              keySecret: simple-nifi-sensitive-property-key
              autoGenerate: true
            authorization:
              opa:
                configMapName: simple-opa
                package: nifi
          nodes:
            roleGroups:
              default:
                replicas: 1
        "#;
        let deserializer = serde_yaml::Deserializer::from_str(input);
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::with::singleton_map_recursive::deserialize(deserializer).unwrap();

        let role_group_configs =
            build_role_group_configs(&nifi, &test_resolved_product_image(), &None).unwrap();
        let merged_config = &role_group_configs
            .get(&NifiRole::Node)
            .and_then(|groups| {
                groups.get(
                    &"default"
                        .parse::<RoleGroupName>()
                        .expect("valid role-group name"),
                )
            })
            .unwrap()
            .config;

        assert_eq!(
            merged_config.affinity,
            StackableAffinity {
                pod_affinity: Some(PodAffinity {
                    preferred_during_scheduling_ignored_during_execution: Some(vec![
                        WeightedPodAffinityTerm {
                            pod_affinity_term: PodAffinityTerm {
                                label_selector: Some(LabelSelector {
                                    match_expressions: None,
                                    match_labels: Some(BTreeMap::from([
                                        ("app.kubernetes.io/name".to_string(), "opa".to_string()),
                                        (
                                            "app.kubernetes.io/instance".to_string(),
                                            "simple-opa".to_string(),
                                        ),
                                        (
                                            "app.kubernetes.io/component".to_string(),
                                            "server".to_string(),
                                        ),
                                    ])),
                                }),
                                topology_key: "kubernetes.io/hostname".to_string(),
                                ..Default::default()
                            },
                            weight: 50,
                        }
                    ]),
                    required_during_scheduling_ignored_during_execution: None,
                }),
                pod_anti_affinity: Some(PodAntiAffinity {
                    preferred_during_scheduling_ignored_during_execution: Some(vec![
                        WeightedPodAffinityTerm {
                            pod_affinity_term: PodAffinityTerm {
                                label_selector: Some(LabelSelector {
                                    match_expressions: None,
                                    match_labels: Some(BTreeMap::from([
                                        ("app.kubernetes.io/name".to_string(), "nifi".to_string(),),
                                        (
                                            "app.kubernetes.io/instance".to_string(),
                                            "simple-nifi".to_string(),
                                        ),
                                        (
                                            "app.kubernetes.io/component".to_string(),
                                            "node".to_string(),
                                        )
                                    ]))
                                }),
                                topology_key: "kubernetes.io/hostname".to_string(),
                                ..Default::default()
                            },
                            weight: 70
                        }
                    ]),
                    required_during_scheduling_ignored_during_execution: None,
                }),
                node_affinity: None,
                node_selector: None,
            }
        );
    }
}
