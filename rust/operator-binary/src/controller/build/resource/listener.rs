use std::str::FromStr;

use stackable_operator::{
    constant,
    crd::listener::v1alpha1::{Listener, ListenerPort, ListenerSpec},
    k8s_openapi::api::core::v1::PersistentVolumeClaim,
    kvp::Labels,
    v2::{
        builder::pod::volume::{
            ListenerReference, listener_operator_volume_source_builder_build_pvc,
        },
        types::{
            kubernetes::{ListenerClassName, ListenerName, PersistentVolumeClaimName},
            operator::RoleName,
        },
    },
};

use crate::{
    controller::{
        ValidatedCluster,
        build::{HTTPS_PORT, HTTPS_PORT_NAME, object_meta, recommended_labels_for_role_resources},
    },
    crd::NifiRole,
};

pub const LISTENER_VOLUME_NAME: &str = "listener";
pub const LISTENER_VOLUME_DIR: &str = "/stackable/listener";

// The listener volume is provisioned as a PVC by the listener-operator; this is its typed name.
constant!(LISTENER_PVC_NAME: PersistentVolumeClaimName = "listener");

pub fn build_group_listener(
    cluster: &ValidatedCluster,
    listener_class: ListenerClassName,
    listener_group_name: ListenerName,
) -> Listener {
    Listener {
        metadata: object_meta(
            cluster,
            listener_group_name.to_string(),
            // The group listener is a role-level (not role-group-level) object, so it carries
            // the role-level recommended labels.
            recommended_labels_for_role_resources(cluster, &NifiRole::Node),
        )
        .build(),
        spec: ListenerSpec {
            class_name: Some(listener_class.to_string()),
            ports: Some(vec![ListenerPort {
                name: HTTPS_PORT_NAME.into(),
                port: HTTPS_PORT.into(),
                protocol: Some("TCP".into()),
            }]),
            ..Default::default()
        },
        status: None,
    }
}

pub fn build_group_listener_pvc(
    group_listener_name: &ListenerName,
    unversioned_recommended_labels: &Labels,
) -> PersistentVolumeClaim {
    listener_operator_volume_source_builder_build_pvc(
        &ListenerReference::Listener(group_listener_name.clone()),
        unversioned_recommended_labels,
        &LISTENER_PVC_NAME,
    )
}

pub fn group_listener_name(cluster: &ValidatedCluster, role_name: &RoleName) -> ListenerName {
    ListenerName::from_str(&format!(
        "{cluster_name}-{role_name}",
        cluster_name = cluster.name
    ))
    .expect(
        "the cluster name and role name form a valid Listener name, because both are length-bounded types whose combined length stays within the Listener name limit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        // Test that dereferencing the constants does not panic.
        let _ = *LISTENER_PVC_NAME;
    }
}
