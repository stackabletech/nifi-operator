//! Computes the NiFi proxy hosts allow-list (`nifi.web.proxy.host`).

use std::collections::HashSet;

use crate::controller::{
    ValidatedCluster,
    build::{
        properties::env_reference,
        resource::statefulset::{LISTENER_DEFAULT_ADDRESS_ENV, LISTENER_DEFAULT_PORT_HTTPS_ENV},
    },
};

/// Computes the comma-separated NiFi proxy hosts, or `"*"` if `hostHeaderCheck.allowAll` is set.
///
/// The proxy hosts allow-list lets external users access NiFi via addresses we cannot predict,
/// so all of them are added to the setting. For more information see
/// <https://nifi.apache.org/docs/nifi-docs/html/administration-guide.html#proxy_configuration>
pub fn compute_proxy_hosts(cluster: &ValidatedCluster) -> String {
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
        address = env_reference(LISTENER_DEFAULT_ADDRESS_ENV),
        port = env_reference(LISTENER_DEFAULT_PORT_HTTPS_ENV)
    )]);
    proxy_hosts.extend(host_header_check.additional_allowed_hosts.iter().cloned());

    let mut proxy_hosts = Vec::from_iter(proxy_hosts);
    proxy_hosts.sort();

    proxy_hosts.join(",")
}

#[cfg(test)]
mod tests {
    use super::compute_proxy_hosts;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

    /// `allow_all` short-circuits to `*` and ignores any additional allowed hosts.
    #[test]
    fn allow_all_returns_wildcard_and_ignores_additional_hosts() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.host_header_check.allow_all = true;
        cluster
            .cluster_config
            .host_header_check
            .additional_allowed_hosts = vec!["ignored.example.com".to_string()];

        assert_eq!(compute_proxy_hosts(&cluster), "*");
    }

    /// Without `allow_all`, the listener placeholder is always present and user-provided hosts are
    /// merged in, sorted and comma-joined.
    #[test]
    fn explicit_hosts_include_listener_placeholder() {
        let mut cluster = minimal_validated_cluster();
        cluster.cluster_config.host_header_check.allow_all = false;
        cluster
            .cluster_config
            .host_header_check
            .additional_allowed_hosts = vec!["extra.example.com".to_string()];

        let hosts = compute_proxy_hosts(&cluster);

        assert!(
            hosts.contains("${env:LISTENER_DEFAULT_ADDRESS}:${env:LISTENER_DEFAULT_PORT_HTTPS}"),
            "expected the listener placeholder in {hosts}"
        );
        assert!(
            hosts.contains("extra.example.com"),
            "expected the additional host in {hosts}"
        );
    }
}
