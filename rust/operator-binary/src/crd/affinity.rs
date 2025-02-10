use stackable_operator::{
    commons::affinity::{affinity_between_role_pods, StackableAffinityFragment},
    k8s_openapi::api::core::v1::PodAntiAffinity,
};

use crate::crd::{NifiRole, APP_NAME};

pub fn get_affinity(cluster_name: &str, role: &NifiRole) -> StackableAffinityFragment {
    StackableAffinityFragment {
        pod_affinity: None,
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
            api::core::v1::{PodAffinityTerm, PodAntiAffinity, WeightedPodAffinityTerm},
            apimachinery::pkg::apis::meta::v1::LabelSelector,
        },
    };

    use super::*;
    use crate::crd::v1alpha1;

    #[test]
    fn test_affinity_defaults() {
        let input = r#"
        apiVersion: nifi.stackable.tech/v1alpha1
        kind: NifiCluster
        metadata:
          name: simple-nifi
        spec:
          image:
            productVersion: 1.27.0
          clusterConfig:
            authentication:
              - authenticationClass: nifi-admin-credentials-simple
            sensitiveProperties:
              keySecret: simple-nifi-sensitive-property-key
              autoGenerate: true
            zookeeperConfigMapName: simple-nifi-znode
          nodes:
            roleGroups:
              default:
                replicas: 1
        "#;
        let deserializer = serde_yaml::Deserializer::from_str(input);
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::with::singleton_map_recursive::deserialize(deserializer).unwrap();
        let merged_config = nifi.merged_config(&NifiRole::Node, "default").unwrap();

        assert_eq!(
            merged_config.affinity,
            StackableAffinity {
                pod_affinity: None,
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
