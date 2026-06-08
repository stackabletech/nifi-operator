//! Computes the NiFi proxy hosts allow-list (`nifi.web.proxy.host`).

use std::collections::HashSet;

use stackable_operator::utils::cluster_info::KubernetesClusterInfo;

use crate::{controller::validate::ValidatedCluster, crd::HTTPS_PORT, reporting_task};

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
    let mut proxy_hosts = HashSet::from([
        "${env:LISTENER_DEFAULT_ADDRESS}:${env:LISTENER_DEFAULT_PORT_HTTPS}".to_string(),
    ]);
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
