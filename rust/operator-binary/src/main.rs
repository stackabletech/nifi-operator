mod config;
mod controller;

use crate::controller::{CONTROLLER_NAME, OPERATOR_NAME};

use std::sync::Arc;

use clap::Parser;
use futures::stream::StreamExt;
use stackable_nifi_crd::{
    authentication::{NifiAuthenticationConfig, NifiAuthenticationMethod},
    NifiCluster,
};
use stackable_operator::{
    cli::{Command, ProductOperatorRun},
    commons::authentication::AuthenticationClass,
    k8s_openapi::api::{
        apps::v1::StatefulSet,
        core::v1::{ConfigMap, Service},
    },
    kube::{
        api::ListParams,
        runtime::{reflector::ObjectRef, Controller},
        ResourceExt,
    },
    logging::controller::report_controller_reconciled,
    CustomResourceExt,
};

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

#[derive(Parser)]
#[clap(about = built_info::PKG_DESCRIPTION, author = "Stackable GmbH - info@stackable.de")]
struct Opts {
    #[clap(subcommand)]
    cmd: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    match opts.cmd {
        Command::Crd => NifiCluster::print_yaml_schema()?,
        Command::Run(ProductOperatorRun {
            product_config,
            watch_namespace,
            tracing_target,
        }) => {
            stackable_operator::logging::initialize_logging(
                "NIFI_OPERATOR_LOG",
                "nifi-operator",
                tracing_target,
            );
            stackable_operator::utils::print_startup_string(
                built_info::PKG_DESCRIPTION,
                built_info::PKG_VERSION,
                built_info::GIT_VERSION,
                built_info::TARGET,
                built_info::BUILT_TIME_UTC,
                built_info::RUSTC_VERSION,
            );

            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/nifi-operator/config-spec/properties.yaml",
            ])?;

            let client =
                stackable_operator::client::create_client(Some(OPERATOR_NAME.to_string()))
                    .await?;

            let nifi_controller = Controller::new(
                watch_namespace.get_api::<NifiCluster>(&client),
                ListParams::default(),
            );

            let nifi_store_1 = nifi_controller.store();

            nifi_controller
                .owns(
                    watch_namespace.get_api::<Service>(&client),
                    ListParams::default(),
                )
                .owns(
                    watch_namespace.get_api::<StatefulSet>(&client),
                    ListParams::default(),
                )
                .owns(
                    watch_namespace.get_api::<ConfigMap>(&client),
                    ListParams::default(),
                )
                .shutdown_on_signal()
                .watches(
                    client.get_api::<AuthenticationClass>(&()),
                    ListParams::default(),
                    move |authentication_class| {
                        nifi_store_1
                            .state()
                            .into_iter()
                            .filter(move |nifi: &Arc<NifiCluster>| {
                                references_authentication_class(
                                    &nifi.spec.config.authentication,
                                    &authentication_class,
                                )
                            })
                            .map(|superset| ObjectRef::from_obj(&*superset))
                    },
                )
                .run(
                    controller::reconcile_nifi,
                    controller::error_policy,
                    Arc::new(controller::Ctx {
                        client: client.clone(),
                        product_config,
                    }),
                )
                .map(|res| {
                    report_controller_reconciled(&client, &format!("{CONTROLLER_NAME}.{OPERATOR_NAME}"), &res)
                })
                .collect::<()>()
                .await;
        }
    }

    Ok(())
}

fn references_authentication_class(
    authentication_config: &NifiAuthenticationConfig,
    authentication_class: &AuthenticationClass,
) -> bool {
    match &authentication_config.method {
        NifiAuthenticationMethod::AuthenticationClass(authentication_class_in_nifi) => {
            authentication_class_in_nifi == &authentication_class.name_any()
        }
        _ => false,
    }
}
