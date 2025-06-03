use std::{borrow::Cow, collections::HashMap, num::TryFromIntError};

use snafu::Snafu;
use stackable_operator::{
    crd::listener::v1alpha1::Listener, k8s_openapi::api::core::v1::Pod,
    kube::runtime::reflector::ObjectRef, utils::cluster_info::KubernetesClusterInfo,
};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("unable to get {listener} (for {pod})"))]
    GetPodListener {
        source: stackable_operator::client::Error,
        listener: ObjectRef<Listener>,
        pod: ObjectRef<Pod>,
    },

    #[snafu(display("{listener} (for {pod}) has no address"))]
    PodListenerHasNoAddress {
        listener: ObjectRef<Listener>,
        pod: ObjectRef<Pod>,
    },

    #[snafu(display("port {port} ({port_name:?}) is out of bounds, must be within {range:?}", range = 0..=u16::MAX))]
    PortOutOfBounds {
        source: TryFromIntError,
        port_name: String,
        port: i32,
    },
}

/// Reference to a single `Pod` that is a component of the product cluster
///
/// Used for service discovery.
#[derive(Debug)]
pub struct PodRef {
    pub namespace: String,
    pub role_group_service_name: String,
    pub pod_name: String,
    pub fqdn_override: Option<String>,
    pub ports: HashMap<String, u16>,
}

impl PodRef {
    pub fn fqdn(&self, cluster_info: &KubernetesClusterInfo) -> Cow<str> {
        self.fqdn_override.as_deref().map_or_else(
            || {
                Cow::Owned(format!(
                    "{pod_name}.{role_group_service_name}.{namespace}.svc.{cluster_domain}",
                    pod_name = self.pod_name,
                    role_group_service_name = self.role_group_service_name,
                    namespace = self.namespace,
                    cluster_domain = cluster_info.cluster_domain,
                ))
            },
            Cow::Borrowed,
        )
    }
}
