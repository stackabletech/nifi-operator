//! The dereference step in the NifiCluster controller
//!
//! Fetches all Kubernetes objects referenced by the NifiCluster spec and returns
//! them in [`DereferencedObjects`].

use std::collections::HashSet;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::client::Client;

use crate::{
    crd::{HTTPS_PORT, v1alpha1},
    reporting_task,
    security::{
        authentication::{self, DereferencedAuthenticationClasses},
        authorization::{self as authorization_mod, DereferencedAuthorization},
    },
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("failed to dereference NiFi authentication classes"))]
    DereferenceAuthenticationClasses { source: authentication::Error },

    #[snafu(display("failed to dereference NiFi authorization config"))]
    DereferenceAuthorization { source: authorization_mod::Error },

    #[snafu(display("failed to build reporting task service name"))]
    ReportingTask {
        source: crate::reporting_task::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Kubernetes objects referenced from the [`v1alpha1::NifiCluster`] spec, already fetched.
pub struct DereferencedObjects {
    pub authentication_classes: DereferencedAuthenticationClasses,
    pub authorization: DereferencedAuthorization,
    /// Comma-separated NiFi proxy hosts, or `"*"` if
    /// `spec.clusterConfig.hostHeaderCheck.allowAll` is set. Computed here so all remote
    /// lookups live in one place; previously this ran once per rolegroup inside the
    /// reconcile loop.
    pub proxy_hosts: String,
}

/// Fetches all Kubernetes objects referenced from the [`v1alpha1::NifiCluster`] spec.
pub async fn dereference(
    client: &Client,
    nifi: &v1alpha1::NifiCluster,
) -> Result<DereferencedObjects> {
    let namespace = nifi
        .metadata
        .namespace
        .as_deref()
        .context(ObjectHasNoNamespaceSnafu)?;

    let authentication_classes = DereferencedAuthenticationClasses::dereference(nifi, client)
        .await
        .context(DereferenceAuthenticationClassesSnafu)?;

    let authorization = DereferencedAuthorization::dereference(
        &nifi.spec.cluster_config.authorization,
        client,
        namespace,
    )
    .await
    .context(DereferenceAuthorizationSnafu)?;

    let proxy_hosts = compute_proxy_hosts(client, nifi)?;

    Ok(DereferencedObjects {
        authentication_classes,
        authorization,
        proxy_hosts,
    })
}

fn compute_proxy_hosts(client: &Client, nifi: &v1alpha1::NifiCluster) -> Result<String> {
    let host_header_check = &nifi.spec.cluster_config.host_header_check;

    if host_header_check.allow_all {
        tracing::info!(
            "spec.clusterConfig.hostHeaderCheck.allowAll is set to true. All proxy hosts will be allowed."
        );
        if !host_header_check.additional_allowed_hosts.is_empty() {
            tracing::info!(
                "spec.clusterConfig.hostHeaderCheck.additionalAllowedHosts is ignored and only '*' is added to the allow-list."
            )
        }
        return Ok("*".to_string());
    }

    // Address and port are injected from the listener volume during the prepare container
    let mut proxy_hosts = HashSet::from([
        "${env:LISTENER_DEFAULT_ADDRESS}:${env:LISTENER_DEFAULT_PORT_HTTPS}".to_string(),
    ]);
    proxy_hosts.extend(host_header_check.additional_allowed_hosts.iter().cloned());

    // Reporting task only exists for NiFi 1.x
    if nifi.spec.image.product_version().starts_with("1.") {
        let reporting_task_service_name = reporting_task::build_reporting_task_fqdn_service_name(
            nifi,
            &client.kubernetes_cluster_info,
        )
        .context(ReportingTaskSnafu)?;

        proxy_hosts.insert(format!("{reporting_task_service_name}:{HTTPS_PORT}"));
    }

    let mut proxy_hosts = Vec::from_iter(proxy_hosts);
    proxy_hosts.sort();

    Ok(proxy_hosts.join(","))
}
