//! Builder for `state-management.xml`.

use crate::{config::NifiRepository, crd::v1alpha1::NifiClusteringBackend};

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
    use super::*;

    #[test]
    fn test_build_state_management_xml_kubernetes() {
        let xml = build(&NifiClusteringBackend::Kubernetes {});
        assert!(xml.contains("kubernetes-provider"));
        assert!(xml.contains("KubernetesConfigMapStateProvider"));
        assert!(xml.contains("${env:STACKLET_NAME}"));
        assert!(!xml.contains("zk-provider"));
    }

    #[test]
    fn test_build_state_management_xml_zookeeper() {
        let xml = build(&NifiClusteringBackend::ZooKeeper {
            zookeeper_config_map_name: "my-zk".to_string(),
        });
        assert!(xml.contains("zk-provider"));
        assert!(xml.contains("ZooKeeperStateProvider"));
        assert!(xml.contains("${env:ZOOKEEPER_HOSTS}"));
        assert!(xml.contains("${env:ZOOKEEPER_CHROOT}"));
        assert!(!xml.contains("kubernetes-provider"));
    }
}
