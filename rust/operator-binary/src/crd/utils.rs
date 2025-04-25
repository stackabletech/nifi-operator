use std::{borrow::Cow, collections::HashMap, num::TryFromIntError};

use futures::future::try_join_all;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    commons::listener::Listener, k8s_openapi::api::core::v1::Pod,
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

pub async fn get_listener_podrefs(
    client: &stackable_operator::client::Client,
    pod_refs: Vec<PodRef>,
    listener_prefix: &str,
) -> Result<Vec<PodRef>, Error> {
    try_join_all(pod_refs.into_iter().map(|pod_ref| async {
        // N.B. use the naming convention for ephemeral listener volumes as we
        // have defined all listeners to be so.
        let listener_name = format!("{}-{listener_prefix}", pod_ref.pod_name);
        let listener_ref = || ObjectRef::<Listener>::new(&listener_name).within(&pod_ref.namespace);
        let pod_obj_ref = || ObjectRef::<Pod>::new(&pod_ref.pod_name).within(&pod_ref.namespace);
        let listener = client
            .get::<Listener>(&listener_name, &pod_ref.namespace)
            .await
            .context(GetPodListenerSnafu {
                listener: listener_ref(),
                pod: pod_obj_ref(),
            })?;
        let listener_address = listener
            .status
            .and_then(|s| s.ingress_addresses?.into_iter().next())
            .context(PodListenerHasNoAddressSnafu {
                listener: listener_ref(),
                pod: pod_obj_ref(),
            })?;
        Ok(PodRef {
            fqdn_override: Some(listener_address.address),
            ports: listener_address
                .ports
                .into_iter()
                .map(|(port_name, port)| {
                    let port = u16::try_from(port).context(PortOutOfBoundsSnafu {
                        port_name: &port_name,
                        port,
                    })?;
                    Ok((port_name, port))
                })
                .collect::<Result<_, _>>()?,
            ..pod_ref
        })
    }))
    .await
}
