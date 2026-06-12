use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec},
    kvp::{Annotations, Labels},
    v2::{builder::meta::ownerreference_from_resource, types::operator::RoleGroupName},
};

use crate::{
    controller::ValidatedCluster,
    crd::{HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME},
};

/// The rolegroup headless [`Service`] is a service that allows direct access to the instances of a certain rolegroup
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
pub fn build_rolegroup_headless_service(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> Service {
    Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(cluster)
            .name(
                cluster
                    .resource_names(role_group_name)
                    .headless_service_name()
                    .to_string(),
            )
            .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
            .with_labels(cluster.recommended_labels(role_group_name))
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(headless_service_ports()),
            selector: Some(cluster.role_group_selector(role_group_name).into()),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    }
}

/// The rolegroup metrics [`Service`] is a service that exposes metrics and a prometheus scraping label.
pub fn build_rolegroup_metrics_service(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> Service {
    let product_version = &cluster.image.product_version;
    Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(cluster)
            .name(metrics_service_name(cluster, role_group_name))
            .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
            .with_labels(cluster.recommended_labels(role_group_name))
            .with_labels(prometheus_labels())
            .with_annotations(prometheus_annotations(product_version))
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(vec![metrics_service_port(product_version)]),
            selector: Some(cluster.role_group_selector(role_group_name).into()),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    }
}

/// The name of the rolegroup metrics [`Service`] (`<cluster>-<role>-<rolegroup>-metrics`).
///
/// [`ResourceNames`](stackable_operator::v2::role_group_utils::ResourceNames) has no metrics
/// service name, so it is derived from the qualified role-group name (which equals the StatefulSet
/// name) here, preserving the previous `RoleGroupRef::rolegroup_metrics_service_name` output.
pub(crate) fn metrics_service_name(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> String {
    format!(
        "{qualified}-metrics",
        qualified = cluster.resource_names(role_group_name).stateful_set_name()
    )
}

fn headless_service_ports() -> Vec<ServicePort> {
    vec![ServicePort {
        name: Some(HTTPS_PORT_NAME.into()),
        port: HTTPS_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }]
}

/// Returns the metrics port based on the NiFi version
/// V1: Uses extra port via JMX exporter
/// V2: Uses NiFi HTTP(S) port for metrics
pub fn metrics_service_port(product_version: &str) -> ServicePort {
    if product_version.starts_with("1.") {
        ServicePort {
            name: Some(METRICS_PORT_NAME.to_string()),
            port: METRICS_PORT.into(),
            protocol: Some("TCP".to_string()),
            ..ServicePort::default()
        }
    } else {
        ServicePort {
            name: Some(HTTPS_PORT_NAME.into()),
            port: HTTPS_PORT.into(),
            protocol: Some("TCP".to_string()),
            ..ServicePort::default()
        }
    }
}

/// Common labels for Prometheus
fn prometheus_labels() -> Labels {
    Labels::try_from([("prometheus.io/scrape", "true")]).expect("should be a valid label")
}

/// Common annotations for Prometheus
///
/// These annotations can be used in a ServiceMonitor.
///
/// see also <https://github.com/prometheus-community/helm-charts/blob/prometheus-27.32.0/charts/prometheus/values.yaml#L983-L1036>
fn prometheus_annotations(product_version: &str) -> Annotations {
    let (path, port, scheme) = if product_version.starts_with("1.") {
        ("/metrics", METRICS_PORT, "http")
    } else {
        ("/nifi-api/flow/metrics/prometheus", HTTPS_PORT, "https")
    };

    Annotations::try_from([
        ("prometheus.io/path".to_owned(), path.to_owned()),
        ("prometheus.io/port".to_owned(), port.to_string()),
        ("prometheus.io/scheme".to_owned(), scheme.to_owned()),
        ("prometheus.io/scrape".to_owned(), "true".to_owned()),
    ])
    .expect("should be valid annotations")
}
