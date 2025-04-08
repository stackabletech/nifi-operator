use std::{ops::Deref as _, sync::Arc};

use clap::Parser;
use futures::stream::StreamExt;
use stackable_operator::{
    YamlSchema,
    cli::{Command, ProductOperatorRun, RollingPeriod},
    commons::authentication::AuthenticationClass,
    k8s_openapi::api::{
        apps::v1::StatefulSet,
        core::v1::{ConfigMap, Service},
    },
    kube::{
        core::DeserializeGuard,
        runtime::{
            Controller,
            events::{Recorder, Reporter},
            reflector::ObjectRef,
            watcher,
        },
    },
    logging::controller::report_controller_reconciled,
    shared::yaml::SerializeOptions,
};
use stackable_telemetry::{Tracing, tracing::settings::Settings};
use tracing::level_filters::LevelFilter;

use crate::{
    controller::NIFI_FULL_CONTROLLER_NAME,
    crd::{NifiCluster, v1alpha1},
};

mod config;
mod controller;
mod crd;
mod operations;
mod product_logging;
mod reporting_task;
mod security;

const OPERATOR_NAME: &str = "nifi.stackable.tech";

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/built.rs"));
}

// TODO (@NickLarsenNZ): Change the variable to `CONSOLE_LOG`
pub const ENV_VAR_CONSOLE_LOG: &str = "NIFI_OPERATOR_LOG";

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
        Command::Crd => NifiCluster::merged_crd(NifiCluster::V1Alpha1)?
            .print_yaml_schema(built_info::PKG_VERSION, SerializeOptions::default())?,
        Command::Run(ProductOperatorRun {
            product_config,
            watch_namespace,
            telemetry_arguments,
            cluster_info_opts,
        }) => {
            // NIFI_OPERATOR_LOG
            // "nifi-operator",

            let _tracing_guard = Tracing::builder()
                // TODO (@NickLarsenNZ): Make this a constant
                .service_name("nifi-operator")
                .with_console_output((
                    ENV_VAR_CONSOLE_LOG,
                    LevelFilter::INFO,
                    !telemetry_arguments.no_console_output,
                ))
                // NOTE (@NickLarsenNZ): Before stackable-telemetry was used, the log directory was
                // set via an env: `NIFI_OPERATOR_LOG_DIRECTORY`.
                // See: https://github.com/stackabletech/operator-rs/blob/f035997fca85a54238c8de895389cc50b4d421e2/crates/stackable-operator/src/logging/mod.rs#L40
                // Now it will be `ROLLING_LOGS` (or via `--rolling-logs <DIRECTORY>`).
                .with_file_output(telemetry_arguments.rolling_logs.map(|log_directory| {
                    let rotation_period = telemetry_arguments
                        .rolling_logs_period
                        .unwrap_or(RollingPeriod::Never)
                        .deref()
                        .clone();

                    Settings::builder()
                        .with_environment_variable(ENV_VAR_CONSOLE_LOG)
                        .with_default_level(LevelFilter::INFO)
                        .file_log_settings_builder(log_directory, "tracing-rs.log")
                        .with_rotation_period(rotation_period)
                        .build()
                }))
                .with_otlp_log_exporter((
                    "OTLP_LOG",
                    LevelFilter::DEBUG,
                    telemetry_arguments.otlp_logs,
                ))
                .with_otlp_trace_exporter((
                    "OTLP_TRACE",
                    LevelFilter::DEBUG,
                    telemetry_arguments.otlp_traces,
                ))
                .build()
                .init()?;

            tracing::info!(
                built_info.pkg_version = built_info::PKG_VERSION,
                built_info.git_version = built_info::GIT_VERSION,
                built_info.target = built_info::TARGET,
                built_info.built_time_utc = built_info::BUILT_TIME_UTC,
                built_info.rustc_version = built_info::RUSTC_VERSION,
                "Starting {description}",
                description = built_info::PKG_DESCRIPTION
            );

            let product_config = product_config.load(&[
                "deploy/config-spec/properties.yaml",
                "/etc/stackable/nifi-operator/config-spec/properties.yaml",
            ])?;

            let client = stackable_operator::client::initialize_operator(
                Some(OPERATOR_NAME.to_string()),
                &cluster_info_opts,
            )
            .await?;

            let event_recorder = Arc::new(Recorder::new(client.as_kube_client(), Reporter {
                controller: NIFI_FULL_CONTROLLER_NAME.to_string(),
                instance: None,
            }));

            let nifi_controller = Controller::new(
                watch_namespace.get_api::<DeserializeGuard<v1alpha1::NifiCluster>>(&client),
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
                    client.get_api::<DeserializeGuard<AuthenticationClass>>(&()),
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
                // We can let the reporting happen in the background
                .for_each_concurrent(
                    16, // concurrency limit
                    move |result| {
                        // The event_recorder needs to be shared across all invocations, so that
                        // events are correctly aggregated
                        let event_recorder = event_recorder.clone();
                        async move {
                            report_controller_reconciled(
                                &event_recorder,
                                NIFI_FULL_CONTROLLER_NAME,
                                &result,
                            )
                            .await;
                        }
                    },
                )
                .await;
        }
    }

    Ok(())
}
