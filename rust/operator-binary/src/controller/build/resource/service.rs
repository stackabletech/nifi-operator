use stackable_operator::{
    k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec},
    kvp::Annotations,
    v2::{
        builder::service::{self, Scheme, Scraping},
        types::operator::RoleGroupName,
    },
};

use crate::controller::{
    ValidatedCluster,
    build::{HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME},
};

/// The rolegroup headless [`Service`] is a service that allows direct access to the instances of a certain rolegroup
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
pub fn build_rolegroup_headless_service(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
) -> Service {
    Service {
        metadata: cluster
            .object_meta(
                cluster
                    .resource_names(role_group_name)
                    .headless_service_name()
                    .to_string(),
                role_group_name,
            )
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
        metadata: cluster
            .object_meta(
                cluster
                    .resource_names(role_group_name)
                    .metrics_service_name()
                    .to_string(),
                role_group_name,
            )
            .with_labels(service::prometheus_labels(&Scraping::Enabled))
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

fn headless_service_ports() -> Vec<ServicePort> {
    vec![ServicePort {
        name: Some(HTTPS_PORT_NAME.into()),
        port: HTTPS_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }]
}

/// Returns the metrics port based on the NiFi version.
///
/// NiFi 1.x exposes a dedicated metrics port via the JMX exporter; NiFi 2.x serves metrics on the
/// NiFi HTTP(S) port.
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

/// Common annotations for Prometheus
///
/// These annotations can be used in a ServiceMonitor.
fn prometheus_annotations(product_version: &str) -> Annotations {
    // NiFi 1.x exposes metrics via the JMX exporter over HTTP on a dedicated port; NiFi 2.x serves
    // them over HTTPS on the NiFi API port.
    let (scheme, path, port) = if product_version.starts_with("1.") {
        (Scheme::Http, "/metrics", METRICS_PORT)
    } else {
        (
            Scheme::Https,
            "/nifi-api/flow/metrics/prometheus",
            HTTPS_PORT,
        )
    };

    service::prometheus_annotations(&Scraping::Enabled, &scheme, path, &port)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use pretty_assertions::assert_eq;
    use rstest::rstest;
    use stackable_operator::v2::types::common::Port;

    use super::*;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

    #[rstest]
    // NiFi 1.x exposes metrics on a dedicated JMX-exporter port ...
    #[case("1.28.1", METRICS_PORT_NAME, METRICS_PORT)]
    // ... while NiFi 2.x serves them on the HTTPS port.
    #[case("2.9.0", HTTPS_PORT_NAME, HTTPS_PORT)]
    fn metrics_service_port_depends_on_version(
        #[case] product_version: &str,
        #[case] expected_name: &str,
        #[case] expected_port: Port,
    ) {
        let port = metrics_service_port(product_version);
        assert_eq!(Some(expected_name.to_string()), port.name);
        assert_eq!(i32::from(expected_port), port.port);
    }

    #[test]
    fn headless_service_is_cluster_ip_none_with_https_port() {
        let cluster = minimal_validated_cluster();
        let rg = RoleGroupName::from_str("default").expect("valid role-group name");

        let spec = build_rolegroup_headless_service(&cluster, &rg)
            .spec
            .expect("headless service must have a spec");

        assert_eq!(Some("ClusterIP".to_string()), spec.type_);
        assert_eq!(Some("None".to_string()), spec.cluster_ip);

        let ports = spec.ports.expect("headless service must expose ports");
        assert_eq!(1, ports.len());
        assert_eq!(Some(HTTPS_PORT_NAME.to_string()), ports[0].name);
        assert_eq!(i32::from(HTTPS_PORT), ports[0].port);
    }
}
