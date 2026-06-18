//! Computes the NiFi proxy hosts allow-list (`nifi.web.proxy.host`).

use std::collections::HashSet;

use stackable_operator::utils::cluster_info::KubernetesClusterInfo;

use crate::controller::{
    ValidatedCluster,
    build::{HTTPS_PORT, properties::env_reference, resource::reporting_task},
};

/// Computes the comma-separated NiFi proxy hosts, or `"*"` if `hostHeaderCheck.allowAll` is set.
///
/// The proxy hosts allow-list lets external users access NiFi via addresses we cannot predict,
/// so all of them are added to the setting. For more information see
/// <https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#proxy_configuration>
pub fn compute_proxy_hosts(
    cluster: &ValidatedCluster,
    cluster_info: &KubernetesClusterInfo,
) -> String {
    let host_header_check = &cluster.cluster_config.host_header_check;

    if host_header_check.allow_all {
        tracing::info!(
            "spec.clusterConfig.hostHeaderCheck.allowAll is set to true. All proxy hosts will be allowed."
        );
        if !host_header_check.additional_allowed_hosts.is_empty() {
            tracing::info!(
                "spec.clusterConfig.hostHeaderCheck.additionalAllowedHosts is ignored and only '*' is added to the allow-list."
            )
        }
        return "*".to_string();
    }

    // Address and port are injected from the listener volume during the prepare container
    let mut proxy_hosts = HashSet::from([format!(
        "{address}:{port}",
        address = env_reference("LISTENER_DEFAULT_ADDRESS"),
        port = env_reference("LISTENER_DEFAULT_PORT_HTTPS")
    )]);
    proxy_hosts.extend(host_header_check.additional_allowed_hosts.iter().cloned());

    // Reporting task only exists for NiFi 1.x
    if cluster.image.product_version.starts_with("1.") {
        let reporting_task_service_name = reporting_task::build_reporting_task_fqdn_service_name(
            cluster.name.as_ref(),
            &cluster.namespace,
            cluster_info,
        );

        proxy_hosts.insert(format!("{reporting_task_service_name}:{HTTPS_PORT}"));
    }

    let mut proxy_hosts = Vec::from_iter(proxy_hosts);
    proxy_hosts.sort();

    proxy_hosts.join(",")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use stackable_operator::{
        commons::networking::DomainName, utils::cluster_info::KubernetesClusterInfo,
    };

    use super::compute_proxy_hosts;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

    fn cluster_info() -> KubernetesClusterInfo {
        KubernetesClusterInfo {
            cluster_domain: DomainName::from_str("cluster.local").expect("valid cluster domain"),
        }
    }

    /// `allow_all` short-circuits to `*` and ignores any additional allowed hosts.
    #[test]
    fn allow_all_returns_wildcard_and_ignores_additional_hosts() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.host_header_check.allow_all = true;
        cluster
            .cluster_config
            .host_header_check
            .additional_allowed_hosts = vec!["ignored.example.com".to_string()];

        assert_eq!(compute_proxy_hosts(&cluster, &cluster_info()), "*");
    }

    /// Without `allow_all`, the listener placeholder is always present and user-provided hosts are
    /// merged in, sorted and comma-joined. On NiFi 2.x no reporting-task FQDN is added.
    #[test]
    fn explicit_hosts_include_listener_placeholder_on_nifi_2() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.host_header_check.allow_all = false;
        cluster
            .cluster_config
            .host_header_check
            .additional_allowed_hosts = vec!["extra.example.com".to_string()];

        let hosts = compute_proxy_hosts(&cluster, &cluster_info());

        assert!(
            hosts.contains("${env:LISTENER_DEFAULT_ADDRESS}:${env:LISTENER_DEFAULT_PORT_HTTPS}"),
            "expected the listener placeholder in {hosts}"
        );
        assert!(
            hosts.contains("extra.example.com"),
            "expected the additional host in {hosts}"
        );
        // No reporting-task FQDN on NiFi 2.x.
        assert!(
            !hosts.contains("cluster.local"),
            "did not expect a reporting-task FQDN on NiFi 2.x in {hosts}"
        );
    }

    /// On NiFi 1.x the reporting-task FQDN (which embeds the cluster domain) is added.
    #[test]
    fn nifi_1_adds_reporting_task_fqdn() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.host_header_check.allow_all = false;
        cluster.image.product_version = "1.27.0".to_string();

        let hosts = compute_proxy_hosts(&cluster, &cluster_info());

        assert!(
            hosts.contains("cluster.local"),
            "expected the reporting-task FQDN on NiFi 1.x in {hosts}"
        );
    }
}
