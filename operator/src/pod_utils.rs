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

/// Build a pod which represents a NiFi node.
///
/// # Arguments
/// * `resource` - NifiCluster
/// * `node_name` - The name if the node
/// * `pod_name` - The name of the pod
/// * `cm_name` - The name of the config map
/// * `labels` - The pod labels to set
///
pub fn build_pod(
    resource: &NifiCluster,
    node_name: &str,
    pod_name: &str,
    cm_name: &str,
    labels: BTreeMap<String, String>,
) -> Result<Pod, Error> {
    Ok(Pod {
        metadata: metadata::build_metadata(pod_name.to_string(), Some(labels), resource, true)?,
        spec: Some(PodSpec {
            node_name: Some(node_name.to_string()),
            tolerations: Some(stackable_operator::krustlet::create_tolerations()),
            containers: build_containers(&resource.spec, &resource.spec.version.to_string()),
            volumes: Some(build_volumes(&cm_name)),
            ..PodSpec::default()
        }),
        ..Pod::default()
    })
}

/// Build required pod containers
///
/// # Arguments
/// * `spec` - NifiSpec
/// * `version` - The current NiFi version
///
fn build_containers(spec: &NifiSpec, version: &str) -> Vec<Container> {
    vec![Container {
        image: Some(format!("nifi:{}", version)),
        name: "nifi".to_string(),
        command: Some(build_nifi_start_command(&spec)),
        volume_mounts: Some(build_volume_mounts(version)),
        ..Container::default()
    }]
}

/// Create a volume to store the NiFi config files.
///
/// # Arguments
/// * `cm_name` - The config map name where the required NiFi configuration files are located
///
fn build_volumes(cm_name: &str) -> Vec<Volume> {
    vec![Volume {
        name: CONFIG_VOLUME.to_string(),
        config_map: Some(ConfigMapVolumeSource {
            name: Some(cm_name.to_string()),
            ..ConfigMapVolumeSource::default()
        }),
        ..Volume::default()
    }]
}

/// Create volume mounts for the NiFi config files.
///
/// # Arguments
/// * `version` - The current NiFi version
///
fn build_volume_mounts(version: &str) -> Vec<VolumeMount> {
    // TODO: For now we set the mount path to the NiFi package config folder.
    //   This needs to be investigated and changed into an separate config folder.
    //   Related to: https://issues.apache.org/jira/browse/NIFI-5573
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
/// * `version` - The current NiFi version
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
fn build_nifi_start_command(spec: &NifiSpec) -> Vec<String> {
    vec![format!("nifi-{}/bin/nifi.sh run", spec.version.to_string())]
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
