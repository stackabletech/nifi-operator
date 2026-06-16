use std::str::FromStr;

use stackable_operator::{
    crd::listener::v1alpha1::{Listener, ListenerPort, ListenerSpec},
    k8s_openapi::api::core::v1::PersistentVolumeClaim,
    kvp::Labels,
    v2::{
        builder::pod::volume::{
            ListenerReference, listener_operator_volume_source_builder_build_pvc,
        },
        types::kubernetes::{ListenerClassName, ListenerName, PersistentVolumeClaimName},
    },
};

use crate::{
    controller::{ValidatedCluster, build::PLACEHOLDER_LISTENER_ROLE_GROUP},
    crd::{HTTPS_PORT, HTTPS_PORT_NAME},
};

pub const LISTENER_VOLUME_NAME: &str = "listener";
pub const LISTENER_VOLUME_DIR: &str = "/stackable/listener";

pub fn build_group_listener(
    cluster: &ValidatedCluster,
    listener_class: ListenerClassName,
    listener_group_name: ListenerName,
) -> Listener {
    Listener {
        metadata: cluster
            .object_meta(
                listener_group_name.to_string(),
                &PLACEHOLDER_LISTENER_ROLE_GROUP,
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
        &PersistentVolumeClaimName::from_str(LISTENER_VOLUME_NAME)
            .expect("'listener' is a valid PersistentVolumeClaim name"),
    )
}

pub fn group_listener_name(cluster: &ValidatedCluster, role_name: &String) -> ListenerName {
    ListenerName::from_str(&format!(
        "{cluster_name}-{role_name}",
        cluster_name = cluster.name
    ))
    .expect("the cluster name and role name form a valid listener name")
}
