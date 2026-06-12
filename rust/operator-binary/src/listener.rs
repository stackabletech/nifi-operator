use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{
        meta::ObjectMetaBuilder,
        pod::volume::{ListenerOperatorVolumeSourceBuilder, ListenerReference},
    },
    crd::listener::v1alpha1::{Listener, ListenerPort, ListenerSpec},
    k8s_openapi::api::core::v1::PersistentVolumeClaim,
    kube::ResourceExt,
    kvp::Labels,
    v2::builder::meta::ownerreference_from_resource,
};

use crate::{
    controller::ValidatedCluster,
    crd::{HTTPS_PORT, HTTPS_PORT_NAME, v1alpha1},
};

pub const LISTENER_VOLUME_NAME: &str = "listener";
pub const LISTENER_VOLUME_DIR: &str = "/stackable/listener";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build listener volume"))]
    BuildListenerPersistentVolume {
        source: stackable_operator::builder::pod::volume::ListenerOperatorVolumeSourceBuilderError,
    },
}

pub fn build_group_listener(
    cluster: &ValidatedCluster,
    listener_class: String,
    listener_group_name: String,
) -> Result<Listener, Error> {
    Ok(Listener {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(cluster)
            .name(listener_group_name)
            .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
            .with_labels(cluster.recommended_labels_role_level())
            .build(),
        spec: ListenerSpec {
            class_name: Some(listener_class),
            ports: Some(vec![ListenerPort {
                name: HTTPS_PORT_NAME.into(),
                port: HTTPS_PORT.into(),
                protocol: Some("TCP".into()),
            }]),
            ..Default::default()
        },
        status: None,
    })
}

pub fn build_group_listener_pvc(
    group_listener_name: &String,
    unversioned_recommended_labels: &Labels,
) -> Result<PersistentVolumeClaim, Error> {
    ListenerOperatorVolumeSourceBuilder::new(
        &ListenerReference::ListenerName(group_listener_name.to_string()),
        unversioned_recommended_labels,
    )
    .build_pvc(LISTENER_VOLUME_NAME.to_string())
    .context(BuildListenerPersistentVolumeSnafu)
}

pub fn group_listener_name(nifi: &v1alpha1::NifiCluster, role_name: &String) -> String {
    format!("{cluster_name}-{role_name}", cluster_name = nifi.name_any(),)
}
