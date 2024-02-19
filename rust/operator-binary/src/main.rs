mod authentication;
mod config;
mod controller;
mod operations;
mod product_logging;
mod reporting_task;
mod security;

use clap::{crate_description, crate_version, Parser};
use futures::stream::StreamExt;
use stackable_operator::{
    cli::{Command, ProductOperatorRun},
    commons::authentication::AuthenticationClass,
    k8s_openapi::api::{
        apps::v1::StatefulSet,
        core::v1::{ConfigMap, Service},
    },
    kube::runtime::{reflector::ObjectRef, watcher, Controller},
    logging::controller::report_controller_reconciled,
    CustomResourceExt,
};
use std::sync::Arc;

use stackable_nifi_crd::NifiCluster;

use crate::controller::NIFI_CONTROLLER_NAME;

const OPERATOR_NAME: &str = "nifi.stackable.tech";

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
    pub const TARGET_PLATFORM: Option<&str> = option_env!("TARGET");
    pub const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
}

#[derive(Parser)]
#[clap(about, author)]
struct Opts {
    #[clap(subcommand)]
    cmd: Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = Opts::parse();
    match opts.cmd {
        Command::Crd => NifiCluster::print_yaml_schema(built_info::CARGO_PKG_VERSION)?,
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
                crate_description!(),
                crate_version!(),
                built_info::GIT_VERSION,
                built_info::TARGET_PLATFORM.unwrap_or("unknown target"),
                built_info::BUILT_TIME_UTC,
                built_info::RUSTC_VERSION,
            );

            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/nifi-operator/config-spec/properties.yaml",
            ])?;

            let client =
                stackable_operator::client::create_client(Some(OPERATOR_NAME.to_string())).await?;

            let nifi_controller = Controller::new(
                watch_namespace.get_api::<NifiCluster>(&client),
                watcher::Config::default(),
            );

            let nifi_store_1 = nifi_controller.store();

            nifi_controller
                .owns(
                    watch_namespace.get_api::<Service>(&client),
                    watcher::Config::default(),
                )
                .owns(
                    watch_namespace.get_api::<StatefulSet>(&client),
                    watcher::Config::default(),
                )
                .owns(
                    watch_namespace.get_api::<ConfigMap>(&client),
                    watcher::Config::default(),
                )
                .shutdown_on_signal()
                .watches(
                    client.get_api::<AuthenticationClass>(&()),
                    watcher::Config::default(),
                    move |_| {
                        nifi_store_1
                            .state()
                            .into_iter()
                            .map(|nifi| ObjectRef::from_obj(&*nifi))
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
                    report_controller_reconciled(
                        &client,
                        &format!("{NIFI_CONTROLLER_NAME}.{OPERATOR_NAME}"),
                        &res,
                    )
                })
                .collect::<()>()
                .await;
        }
    }

    Ok(())
}
