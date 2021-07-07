use stackable_nifi_crd::NifiSpec;

/// # Arguments
/// * `app_name` - The name of the cluster application (Spark, Kafka ...)
/// * `context_name` - The name of the cluster as specified in the custom resource
/// * `role` - The cluster role (e.g. master, worker, history-server)
/// * `role_group` - The role group of the selector
/// * `node_name` - The node or host name
///
// TODO: Remove (move to) for operator-rs method
pub fn build_pod_name(
    app_name: &str,
    context_name: &str,
    role: &str,
    role_group: &str,
    node_name: &str,
) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        app_name, context_name, role_group, role, node_name
    )
    .to_lowercase()
}

/// Retrieve the config belonging to a role group selector.
///
/// # Arguments
/// * `spec` - The custom resource spec definition to extract the version
///
pub fn build_nifi_start_command(spec: &NifiSpec) -> Vec<String> {
    vec![format!("nifi-{}/bin/nifi.sh run", spec.version.to_string())]
}
