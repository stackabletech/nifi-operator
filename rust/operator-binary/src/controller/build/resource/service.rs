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
    build::{HTTPS_PORT, HTTPS_PORT_NAME},
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
                    .role_group_resource_names(role_group_name)
                    .headless_service_name()
                    .to_string(),
                role_group_name,
            )
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(vec![headless_service_port()]),
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
    Service {
        metadata: cluster
            .object_meta(
                cluster
                    .role_group_resource_names(role_group_name)
                    .metrics_service_name()
                    .to_string(),
                role_group_name,
            )
            .with_labels(service::prometheus_labels(&Scraping::Enabled))
            .with_annotations(prometheus_annotations())
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(vec![metrics_service_port()]),
            selector: Some(cluster.role_group_selector(role_group_name).into()),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    }
}

/// Returns the headless Service port, which exposes the HTTPS UI.
fn headless_service_port() -> ServicePort {
    ServicePort {
        name: Some(HTTPS_PORT_NAME.into()),
        port: HTTPS_PORT.into(),
        protocol: Some("TCP".to_string()),
        ..ServicePort::default()
    }
}

/// Returns the metrics port, which is the same as the headless Service port
pub fn metrics_service_port() -> ServicePort {
    headless_service_port()
}

/// Common annotations for Prometheus
///
/// These annotations can be used in a ServiceMonitor.
fn prometheus_annotations() -> Annotations {
    service::prometheus_annotations(
        &Scraping::Enabled,
        &Scheme::Https,
        "/nifi-api/flow/metrics/prometheus",
        &HTTPS_PORT,
    )
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

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
