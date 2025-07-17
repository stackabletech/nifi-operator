use std::collections::BTreeMap;

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec},
    kvp::{Label, ObjectLabels},
    role_utils::RoleGroupRef,
};

use crate::crd::{HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT, METRICS_PORT_NAME, v1alpha1};

const METRICS_SERVICE_SUFFIX: &str = "metrics";
const HEADLESS_SERVICE_SUFFIX: &str = "headless";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build Metadata"))]
    MetadataBuild {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build Labels"))]
    LabelBuild {
        source: stackable_operator::kvp::LabelError,
    },
}

/// The rolegroup headless [`Service`] is a service that allows direct access to the instances of a certain rolegroup
/// This is mostly useful for internal communication between peers, or for clients that perform client-side load balancing.
pub fn build_rolegroup_headless_service(
    nifi: &v1alpha1::NifiCluster,
    role_group_ref: &RoleGroupRef<v1alpha1::NifiCluster>,
    object_labels: ObjectLabels<v1alpha1::NifiCluster>,
    selector: BTreeMap<String, String>,
) -> Result<Service, Error> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(rolegroup_headless_service_name(
                &role_group_ref.object_name(),
            ))
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(object_labels)
            .context(MetadataBuildSnafu)?
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(headless_service_ports()),
            selector: Some(selector),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// The rolegroup metrics [`Service`] is a service that exposes metrics and a prometheus scraping label.
pub fn build_rolegroup_metrics_service(
    nifi: &v1alpha1::NifiCluster,
    role_group_ref: &RoleGroupRef<v1alpha1::NifiCluster>,
    object_labels: ObjectLabels<v1alpha1::NifiCluster>,
    selector: BTreeMap<String, String>,
    ports: Vec<ServicePort>,
) -> Result<Service, Error> {
    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(rolegroup_metrics_service_name(
                &role_group_ref.object_name(),
            ))
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(object_labels)
            .context(MetadataBuildSnafu)?
            .with_label(Label::try_from(("prometheus.io/scrape", "true")).context(LabelBuildSnafu)?)
            .build(),
        spec: Some(ServiceSpec {
            // Internal communication does not need to be exposed
            type_: Some("ClusterIP".to_string()),
            cluster_ip: Some("None".to_string()),
            ports: Some(ports),
            selector: Some(selector),
            publish_not_ready_addresses: Some(true),
            ..ServiceSpec::default()
        }),
        status: None,
    })
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

/// Returns the metrics rolegroup service name `<cluster>-<role>-<rolegroup>-<METRICS_SERVICE_SUFFIX>`.
pub fn rolegroup_metrics_service_name(role_group_ref_object_name: impl AsRef<str>) -> String {
    let role_group_ref_object_name = role_group_ref_object_name.as_ref();
    format!("{role_group_ref_object_name}-{METRICS_SERVICE_SUFFIX}")
}

/// Returns the headless rolegroup service name `<cluster>-<role>-<rolegroup>-<HEADLESS_SERVICE_SUFFIX>`.
pub fn rolegroup_headless_service_name(role_group_ref_object_name: &str) -> String {
    format!("{role_group_ref_object_name}-{HEADLESS_SERVICE_SUFFIX}")
}
