use std::str::FromStr;

use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    crd::listener::v1alpha1::{Listener, ListenerPort, ListenerSpec},
    k8s_openapi::api::core::v1::PersistentVolumeClaim,
    kvp::Labels,
    v2::{
        builder::{
            meta::ownerreference_from_resource,
            pod::volume::{ListenerReference, listener_operator_volume_source_builder_build_pvc},
        },
        types::kubernetes::{ListenerClassName, ListenerName, PersistentVolumeClaimName},
    },
};

use crate::{
    controller::ValidatedCluster,
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
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(cluster)
            .name(listener_group_name.to_string())
            .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
            .with_labels(cluster.recommended_labels_role_level())
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
