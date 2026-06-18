//! Builder for `state-management.xml`.

use crate::crd::{storage::NifiRepository, v1alpha1::NifiClusteringBackend};

/// State-provider IDs defined by the providers in `state-management.xml`.
pub const LOCAL_STATE_PROVIDER_ID: &str = "local-provider";
pub const ZOOKEEPER_STATE_PROVIDER_ID: &str = "zk-provider";
pub const KUBERNETES_STATE_PROVIDER_ID: &str = "kubernetes-provider";

pub fn build(clustering_backend: &NifiClusteringBackend) -> String {
    // Inert providers are ignored by NiFi itself, but templating still fails if they refer to invalid environment variables,
    // so only include the actually used provider.
    let cluster_provider = match clustering_backend {
        NifiClusteringBackend::ZooKeeper { .. } => {
            r#"<cluster-provider>
              <id>zk-provider</id>
              <class>org.apache.nifi.controller.state.providers.zookeeper.ZooKeeperStateProvider</class>
              <property name="Connect String">${env:ZOOKEEPER_HOSTS}</property>
              <property name="Root Node">${env:ZOOKEEPER_CHROOT}</property>
              <property name="Session Timeout">10 seconds</property>
              <property name="Access Control">Open</property>
            </cluster-provider>"#
        }
        NifiClusteringBackend::Kubernetes {} => {
            r#"<cluster-provider>
              <id>kubernetes-provider</id>
              <class>org.apache.nifi.kubernetes.state.provider.KubernetesConfigMapStateProvider</class>
              <property name="ConfigMap Name Prefix">${env:STACKLET_NAME}</property>
            </cluster-provider>"#
        }
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
        <stateManagement>
          <local-provider>
            <id>local-provider</id>
            <class>org.apache.nifi.controller.state.providers.local.WriteAheadLocalStateProvider</class>
            <property name="Directory">{local_state_path}</property>
            <property name="Always Sync">false</property>
            <property name="Partitions">16</property>
            <property name="Checkpoint Interval">2 mins</property>
          </local-provider>
          {cluster_provider}
        </stateManagement>"#,
        local_state_path = NifiRepository::State.mount_path(),
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use stackable_operator::v2::types::kubernetes::ConfigMapName;

    use super::{super::env_reference, *};
    use crate::controller::build::resource::statefulset::{
        STACKLET_NAME_ENV, ZOOKEEPER_CHROOT_ENV, ZOOKEEPER_HOSTS_ENV,
    };

    #[test]
    fn test_build_state_management_xml_kubernetes() {
        let xml = build(&NifiClusteringBackend::Kubernetes {});
        assert!(xml.contains(KUBERNETES_STATE_PROVIDER_ID));
        assert!(xml.contains("KubernetesConfigMapStateProvider"));
        assert!(xml.contains(&env_reference(STACKLET_NAME_ENV)));
        assert!(!xml.contains(ZOOKEEPER_STATE_PROVIDER_ID));
    }

    #[test]
    fn test_build_state_management_xml_zookeeper() {
        let xml = build(&NifiClusteringBackend::ZooKeeper {
            zookeeper_config_map_name: ConfigMapName::from_str("my-zk")
                .expect("'my-zk' is a valid ConfigMap name"),
        });
        assert!(xml.contains(ZOOKEEPER_STATE_PROVIDER_ID));
        assert!(xml.contains("ZooKeeperStateProvider"));
        assert!(xml.contains(&env_reference(ZOOKEEPER_HOSTS_ENV)));
        assert!(xml.contains(&env_reference(ZOOKEEPER_CHROOT_ENV)));
        assert!(!xml.contains(KUBERNETES_STATE_PROVIDER_ID));
    }
}
