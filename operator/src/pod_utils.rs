use crate::error::Error;
use crate::NifiRole;
use k8s_openapi::api::core::v1::{
    ConfigMapVolumeSource, Container, Pod, PodSpec, Volume, VolumeMount,
};
use stackable_nifi_crd::{NifiCluster, NifiConfig, NifiSpec};
use stackable_operator::labels::{
    APP_COMPONENT_LABEL, APP_INSTANCE_LABEL, APP_ROLE_GROUP_LABEL, APP_VERSION_LABEL,
};
use stackable_operator::metadata;
use std::collections::BTreeMap;

/// Name of the config volume to store configmap data
const CONFIG_VOLUME: &str = "config-volume";

/// Build a pod which represents a Nifi node in the cluster.
///
/// # Arguments
/// * `resource` - NifiCluster
/// * `node_name` - The name if the node
/// * `pod_name` - The name of the pod
/// * `cm_name` - The name of the config map
/// * `version` - The current nifi version
/// * `labels` - The pod labels to set
///
pub fn build_pod(
    resource: &NifiCluster,
    node_name: &str,
    pod_name: &str,
    cm_name: &str,
    version: &str,
    labels: BTreeMap<String, String>,
) -> Result<Pod, Error> {
    let (containers, volumes) = build_containers(&resource.spec, cm_name, version);

    Ok(Pod {
        metadata: metadata::build_metadata(pod_name.to_string(), Some(labels), resource, true)?,
        spec: Some(PodSpec {
            node_name: Some(node_name.to_string()),
            tolerations: Some(stackable_operator::krustlet::create_tolerations()),
            containers,
            volumes: Some(volumes),
            ..PodSpec::default()
        }),
        ..Pod::default()
    })
}

/// Build required pod containers
///
/// # Arguments
/// * `spec` - NifiSpec
/// * `cm_name` - The name of the config map
/// * `version` - The current nifi version
///
fn build_containers(
    spec: &NifiSpec,
    cm_name: &str,
    version: &str,
) -> (Vec<Container>, Vec<Volume>) {
    let containers = vec![Container {
        image: Some(format!("nifi:{}", version)),
        name: "nifi".to_string(),
        command: Some(create_nifi_start_command(&spec)),
        volume_mounts: Some(create_volume_mounts(version)),
        ..Container::default()
    }];

    let volumes = create_volumes(&cm_name);

    (containers, volumes)
}

/// Create a volume to store the nifi config files.
///
/// # Arguments
/// * `cm_name` - The config map name where the required nifi configuration files are located
///
fn create_volumes(cm_name: &str) -> Vec<Volume> {
    vec![Volume {
        name: CONFIG_VOLUME.to_string(),
        config_map: Some(ConfigMapVolumeSource {
            name: Some(cm_name.to_string()),
            ..ConfigMapVolumeSource::default()
        }),
        ..Volume::default()
    }]
}

/// Create volume mounts for the nifi config files.
///
/// # Arguments
/// * `version` - The current nifi version
///
fn create_volume_mounts(version: &str) -> Vec<VolumeMount> {
    vec![VolumeMount {
        mount_path: format!("{}/nifi-{}/conf", "{{packageroot}}", version),
        name: CONFIG_VOLUME.to_string(),
        ..VolumeMount::default()
    }]
}

/// Provide required labels for pods.
///
/// # Arguments
/// * `role` - NifiRole in the cluster
/// * `role_group` - Role group
/// * `name` - The name of the cluster
/// * `version` - The current nifi version
///
pub fn build_labels(
    role: &NifiRole,
    role_group: &str,
    name: &str,
    version: &str,
) -> BTreeMap<String, String> {
    let mut node_labels = BTreeMap::new();
    node_labels.insert(String::from(APP_COMPONENT_LABEL), role.to_string());
    node_labels.insert(String::from(APP_ROLE_GROUP_LABEL), role_group.to_string());
    node_labels.insert(String::from(APP_INSTANCE_LABEL), name.to_string());
    node_labels.insert(String::from(APP_VERSION_LABEL), version.to_string());

    node_labels
}

/// Retrieve the config belonging to a role group selector.
///
/// # Arguments
/// * `spec` - The custom resource spec definition to extract the version
///
fn create_nifi_start_command(spec: &NifiSpec) -> Vec<String> {
    let command = vec![format!("nifi-{}/bin/nifi.sh run", spec.version.to_string())];
    command
}

/// Retrieve the config belonging to a role group selector.
///
/// # Arguments
/// * `role_group` - Role group to identify the selector
/// * `spec` - The custom resource spec definition
///
pub fn get_selector_config(role_group: &str, spec: &NifiSpec) -> Option<NifiConfig> {
    spec.nodes
        .selectors
        .get(role_group)
        .map(|selector| selector.config.clone())
}

/// Retrieve the value of a label in the pod.
///
/// # Arguments
/// * `pod` - Pod which has some labels
/// * `label` - Label key pointing to the value
///
pub fn get_pod_label(pod: &Pod, label: &str) -> Option<String> {
    if let Some(labels) = &pod.metadata.labels {
        return labels.get(label).cloned();
    }
    None
}
