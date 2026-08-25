//! Ensures that `Pod`s are configured and running for each [`v1alpha1::NifiCluster`].
//!
//! This is the controller driver: it runs the
//! `dereference -> validate -> build -> apply -> update_status` pipeline. The validated cluster
//! type and the resource builders live under the [`crate::controller`] module tree; this file is
//! kept next to `main.rs` for consistency with the other Stackable operators.

use std::sync::Arc;

use const_format::concatcp;
use snafu::{ResultExt, Snafu};
use stackable_operator::{
    cli::OperatorEnvironmentOptions,
    client::Client,
    cluster_resources::ClusterResourceApplyStrategy,
    kube::{
        Resource,
        core::{DeserializeGuard, error_boundary},
        runtime::controller::Action,
    },
    logging::controller::ReconcilerError,
    shared::time::Duration,
};
use strum::{EnumDiscriminants, IntoStaticStr};

use crate::{
    NIFI_OPERATOR_NAME,
    controller::{
        apply::{self, Applier},
        build, dereference,
        update_status::{self, update_status},
        validate,
    },
    crd::v1alpha1,
};

pub const NIFI_CONTROLLER_NAME: &str = "nificluster";
pub const NIFI_FULL_CONTROLLER_NAME: &str =
    concatcp!(NIFI_CONTROLLER_NAME, '.', NIFI_OPERATOR_NAME);

pub struct Ctx {
    pub client: Client,
    pub operator_environment: OperatorEnvironmentOptions,
}

#[derive(Snafu, Debug, EnumDiscriminants)]
#[strum_discriminants(derive(IntoStaticStr))]
pub enum Error {
    #[snafu(display("NifiCluster object is invalid"))]
    InvalidNifiCluster {
        source: error_boundary::InvalidObject,
    },

    #[snafu(display("failed to dereference resources"))]
    Dereference { source: dereference::Error },

    #[snafu(display("failed to validate cluster"))]
    ValidateCluster { source: validate::Error },

    #[snafu(display("failed to build the Kubernetes resources"))]
    BuildResources { source: build::Error },

    #[snafu(display("failed to apply the Kubernetes resources"))]
    ApplyResources { source: apply::Error },

    #[snafu(display("failed to update the cluster status"))]
    UpdateStatus { source: update_status::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

impl ReconcilerError for Error {
    fn category(&self) -> &'static str {
        ErrorDiscriminants::from(self).into()
    }
}

pub async fn reconcile_nifi(
    nifi: Arc<DeserializeGuard<v1alpha1::NifiCluster>>,
    ctx: Arc<Ctx>,
) -> Result<Action> {
    tracing::info!("Starting reconcile");

    if nifi.meta().deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }

    let nifi = nifi
        .0
        .as_ref()
        .map_err(error_boundary::InvalidObject::clone)
        .context(InvalidNifiClusterSnafu)?;

    let client = &ctx.client;

    // dereference (client required)
    let dereferenced_objects = dereference::dereference(client, nifi)
        .await
        .context(DereferenceSnafu)?;

    // validate (no Kubernetes API calls required)
    let validated_cluster =
        validate::validate(nifi, &dereferenced_objects, &ctx.operator_environment)
            .context(ValidateClusterSnafu)?;

    // build (no Kubernetes API calls required)
    let resources = build::build(&validated_cluster).context(BuildResourcesSnafu)?;

    // apply (client required)
    let applied = Applier::new(
        client,
        &validated_cluster,
        ClusterResourceApplyStrategy::from(&nifi.spec.cluster_operation),
        &nifi.spec.object_overrides,
    )
    .apply(resources)
    .await
    .context(ApplyResourcesSnafu)?;

    // update status (client required)
    update_status(client, nifi, &validated_cluster, &applied)
        .await
        .context(UpdateStatusSnafu)?;

    Ok(Action::await_change())
}

pub fn error_policy(
    _obj: Arc<DeserializeGuard<v1alpha1::NifiCluster>>,
    error: &Error,
    _ctx: Arc<Ctx>,
) -> Action {
    match error {
        // root object is invalid, will be requeued when modified anyway
        Error::InvalidNifiCluster { .. } => Action::await_change(),

        _ => Action::requeue(*Duration::from_secs(10)),
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use stackable_operator::{
        cli::OperatorEnvironmentOptions,
        client::Client,
        commons::networking::DomainName,
        kube::{Client as KubeClient, Config, runtime::controller::Action},
        utils::cluster_info::KubernetesClusterInfo,
    };

    use crate::nifi_controller::{Ctx, reconcile_nifi};

    /// The client points at a closed port, so any API call would fail the reconciliation: an `Ok`
    /// proves that a cluster being deleted returns before the reconciler touches the Kubernetes
    /// API, and because the spec is invalid, before the `DeserializeGuard` is unwrapped.
    #[test]
    fn reconcile_exits_early_for_deleted_cluster() {
        let nifi = serde_yaml::from_str(
            r#"
apiVersion: nifi.stackable.tech/v1alpha1
kind: NifiCluster
metadata:
  name: nifi
  namespace: default
  deletionTimestamp: "2026-08-14T12:00:00Z"
spec: {}
"#,
        )
        .expect("YAML parses; the invalid spec is captured inside the DeserializeGuard");

        let action = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread tokio runtime")
            .block_on(async {
                let ctx = Arc::new(Ctx {
                    client: Client::new(
                        KubeClient::try_from(Config::new(
                            "http://127.0.0.1:1".parse().expect("valid static URI"),
                        ))
                        .expect("client from static config"),
                        None,
                        "default".to_owned(),
                        KubernetesClusterInfo {
                            cluster_domain: DomainName::from_str("cluster.local")
                                .expect("valid cluster domain"),
                        },
                    ),
                    operator_environment: OperatorEnvironmentOptions {
                        operator_namespace: "stackable-operators".to_owned(),
                        operator_service_name: "nifi-operator".to_owned(),
                        image_repository: "oci.stackable.tech/sdp".to_owned(),
                    },
                });

                reconcile_nifi(Arc::new(nifi), ctx).await
            })
            .expect("a deleted cluster reconciles without any API call");

        assert_eq!(action, Action::await_change());
    }
}
