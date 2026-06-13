//! Builds the rolegroup [`StatefulSet`] that runs a NiFi node role group.

use std::collections::BTreeMap;

use indoc::formatdoc;
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{
        self,
        meta::ObjectMetaBuilder,
        pod::{
            PodBuilder, container::ContainerBuilder, resources::ResourceRequirementsBuilder,
            security::PodSecurityContextBuilder, volume::SecretFormat,
        },
    },
    commons::product_image_selection::ResolvedProductImage,
    constants::RESTART_CONTROLLER_ENABLED_LABEL,
    crd::{authentication::oidc::v1alpha1::AuthenticationProvider, git_sync},
    k8s_openapi::{
        DeepMerge,
        api::{
            apps::v1::{StatefulSet, StatefulSetSpec, StatefulSetUpdateStrategy},
            core::v1::{
                ConfigMapKeySelector, ConfigMapVolumeSource, EmptyDirVolumeSource, EnvVar,
                EnvVarSource, ObjectFieldSelector, PersistentVolumeClaim, Probe,
                SecretVolumeSource, TCPSocketAction, Volume,
            },
        },
        apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
    },
    kube::ResourceExt,
    memory::{BinaryMultiple, MemoryQuantity},
    product_logging::{
        self,
        framework::{
            LoggingError, create_vector_shutdown_file_command, remove_vector_shutdown_file_command,
        },
        spec::{
            ConfigMapLogConfig, ContainerLogConfig, ContainerLogConfigChoice,
            CustomContainerLogConfig,
        },
    },
    utils::{COMMON_BASH_TRAP_FUNCTIONS, cluster_info::KubernetesClusterInfo},
    v2::{builder::meta::ownerreference_from_resource, types::operator::RoleGroupName},
};

use crate::{
    controller::{
        ValidatedCluster, ValidatedRoleGroupConfig,
        build::{
            graceful_shutdown::add_graceful_shutdown_config,
            resource::{
                listener::{
                    LISTENER_VOLUME_DIR, LISTENER_VOLUME_NAME, build_group_listener_pvc,
                    group_listener_name,
                },
                reporting_task::build_reporting_task_service_name,
            },
        },
    },
    crd::{
        BALANCE_PORT, BALANCE_PORT_NAME, Container, HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT,
        METRICS_PORT_NAME, NifiConfig, NifiRole, NifiRoleType, PROTOCOL_PORT, PROTOCOL_PORT_NAME,
        STACKABLE_LOG_CONFIG_DIR, STACKABLE_LOG_DIR,
        authorization::NifiAccessPolicyProvider,
        constants::{NIFI_CONFIG_DIRECTORY, NIFI_PYTHON_WORKING_DIRECTORY},
        storage::{NifiRepository, PERSISTENT_REPOSITORIES},
        v1alpha1,
    },
    security::{
        authentication::{
            NifiAuthenticationConfig, STACKABLE_SERVER_TLS_DIR, STACKABLE_TLS_STORE_PASSWORD,
        },
        authorization::{self, OPA_TLS_MOUNT_PATH, ResolvedNifiAuthorizationConfig},
        build_tls_volume,
        tls::{KEYSTORE_NIFI_CONTAINER_MOUNT, KEYSTORE_VOLUME_NAME, TRUSTSTORE_VOLUME_NAME},
    },
};

/// Errors that can occur while building the rolegroup [`StatefulSet`].
#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("missing secret lifetime"))]
    MissingSecretLifetime,

    #[snafu(display("object defines no name"))]
    ObjectHasNoName,

    #[snafu(display("illegal container name: [{container_name}]"))]
    IllegalContainerName {
        source: stackable_operator::builder::pod::container::Error,
        container_name: String,
    },

    #[snafu(display("vector agent is enabled but vector aggregator ConfigMap is missing"))]
    VectorAggregatorConfigMapMissing,

    #[snafu(display("failed to add Authentication Volumes and VolumeMounts"))]
    AddAuthVolumes {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("security failure"))]
    Security { source: crate::security::Error },

    #[snafu(display("failed to configure logging"))]
    ConfigureLogging { source: LoggingError },

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },

    #[snafu(display("failed to configure graceful shutdown"))]
    GracefulShutdown {
        source: crate::controller::build::graceful_shutdown::Error,
    },

    #[snafu(display("failed to configure listener"))]
    ListenerConfiguration {
        source: crate::controller::build::resource::listener::Error,
    },

    #[snafu(display("failed to build authorization configuration"))]
    AuthorizationConfiguration { source: authorization::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

const USERDATA_MOUNTPOINT: &str = "/stackable/userdata";

/// Volume providing the rendered NiFi config (the `conf` ConfigMap), mounted into the prepare
/// container which templates it into [`ACTIVE_CONFIG_VOLUME_NAME`].
const CONFIG_VOLUME_NAME: &str = "conf";
const CONFIG_VOLUME_MOUNT: &str = "/conf";

/// `emptyDir` holding the live config templated by the prepare container and shared with the NiFi
/// container.
const ACTIVE_CONFIG_VOLUME_NAME: &str = "activeconf";

/// Volume holding the generated sensitive-properties key.
const SENSITIVE_PROPERTY_VOLUME_NAME: &str = "sensitiveproperty";
const SENSITIVE_PROPERTY_VOLUME_MOUNT: &str = "/stackable/sensitiveproperty";

/// Volume providing the log config (logback/log4j) ConfigMap.
const LOG_CONFIG_VOLUME_NAME: &str = "log-config";

/// Volume the NiFi logs are written to and shared with the Vector sidecar (also used by the
/// git-sync container, see [`crate::controller::build::git_sync`]).
pub(crate) const LOG_VOLUME_NAME: &str = "log";

/// The rolegroup [`StatefulSet`] runs the rolegroup, as configured by the administrator.
///
/// The [`Pod`](`stackable_operator::k8s_openapi::api::core::v1::Pod`)s are accessible through the
/// corresponding [`stackable_operator::k8s_openapi::api::core::v1::Service`] (from [`build_rolegroup_headless_service`]).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_node_rolegroup_statefulset(
    nifi: &v1alpha1::NifiCluster,
    cluster: &ValidatedCluster,
    resolved_product_image: &ResolvedProductImage,
    cluster_info: &KubernetesClusterInfo,
    role_group_name: &RoleGroupName,
    role: &NifiRoleType,
    rg: &ValidatedRoleGroupConfig,
    authentication_config: &NifiAuthenticationConfig,
    authorization_config: &ResolvedNifiAuthorizationConfig,
    rolling_update_supported: bool,
    replicas: Option<i32>,
    service_account_name: &str,
    git_sync_resources: &git_sync::v1alpha2::GitSyncResources,
) -> Result<StatefulSet> {
    tracing::debug!("Building statefulset");

    // Type-safe names for this role group's resources (StatefulSet, ConfigMap, headless Service).
    let resource_names = cluster.resource_names(role_group_name);

    // The validated, merged `NifiConfig` is the single source of truth; the ConfigMap builder
    // sources the same `rg.config`.
    let merged_config = &rg.config;

    let mut env_vars: Vec<EnvVar> = rg.env_overrides.clone().into();

    // we need the POD_NAME env var to overwrite `nifi.cluster.node.address` later
    env_vars.push(EnvVar {
        name: "POD_NAME".to_string(),
        value_from: Some(EnvVarSource {
            field_ref: Some(ObjectFieldSelector {
                api_version: Some("v1".to_string()),
                field_path: "metadata.name".to_string(),
            }),
            ..EnvVarSource::default()
        }),
        ..EnvVar::default()
    });

    // Needed for the `containerdebug` process to log it's tracing information to.
    env_vars.push(EnvVar {
        name: "CONTAINERDEBUG_LOG_DIRECTORY".to_string(),
        value: Some(format!("{STACKABLE_LOG_DIR}/containerdebug")),
        ..Default::default()
    });

    env_vars.push(EnvVar {
        name: "STACKLET_NAME".to_string(),
        value: Some(nifi.name_unchecked().to_string()),
        ..Default::default()
    });

    match &nifi.spec.cluster_config.clustering_backend {
        v1alpha1::NifiClusteringBackend::ZooKeeper {
            zookeeper_config_map_name,
        } => {
            let zookeeper_env_var = |name: &str| EnvVar {
                name: name.to_string(),
                value_from: Some(EnvVarSource {
                    config_map_key_ref: Some(ConfigMapKeySelector {
                        name: zookeeper_config_map_name.to_string(),
                        key: name.to_string(),
                        ..ConfigMapKeySelector::default()
                    }),
                    ..EnvVarSource::default()
                }),
                ..EnvVar::default()
            };
            env_vars.push(zookeeper_env_var("ZOOKEEPER_HOSTS"));
            env_vars.push(zookeeper_env_var("ZOOKEEPER_CHROOT"));
        }
        v1alpha1::NifiClusteringBackend::Kubernetes {} => {}
    }

    if let NifiAuthenticationConfig::Oidc { oidc, .. } = authentication_config {
        env_vars.extend(AuthenticationProvider::client_credentials_env_var_mounts(
            oidc.client_credentials_secret_ref.clone(),
        ));
    }

    env_vars.extend(authorization_config.get_env_vars());

    let node_address = format!(
        "$POD_NAME.{service_name}.{namespace}.svc.{cluster_domain}",
        service_name = resource_names.headless_service_name(),
        namespace = cluster.namespace,
        cluster_domain = cluster_info.cluster_domain,
    );

    let sensitive_key_secret = &nifi.spec.cluster_config.sensitive_properties.key_secret;

    let prepare_container_name = Container::Prepare.to_string();
    let mut prepare_args = vec![];

    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = merged_config.logging.containers.get(&Container::Prepare)
    {
        prepare_args.push(product_logging::framework::capture_shell_output(
            STACKABLE_LOG_DIR,
            &prepare_container_name,
            log_config,
        ));
    }

    // Note(sbernauer): In https://github.com/stackabletech/issues/issues/764 we migrated all usages
    // of keytool to our own cert-utils tool. As it uses the same code as secret-operator, it also
    // uses RC2. Thus, the keytool usage here LGTM (no alias trickery) and has my nod of approval.
    prepare_args.extend(vec![
        // The source directory is a secret-op mount and we do not want to write / add anything in there
        // Therefore we import all the contents to a truststore in "writable" empty dirs.
        // Keytool is only barking if a password is not set for the destination truststore (which we set)
        // and do provide an empty password for the source truststore coming from the secret-operator.
        // Using no password will result in a warning.
        format!("echo Importing {KEYSTORE_NIFI_CONTAINER_MOUNT}/keystore.p12 to {STACKABLE_SERVER_TLS_DIR}/keystore.p12"),
        format!("cp {KEYSTORE_NIFI_CONTAINER_MOUNT}/keystore.p12 {STACKABLE_SERVER_TLS_DIR}/keystore.p12"),
        format!("echo Importing {KEYSTORE_NIFI_CONTAINER_MOUNT}/truststore.p12 to {STACKABLE_SERVER_TLS_DIR}/truststore.p12"),
        // secret-operator currently encrypts keystores with RC2, which NiFi is unable to read: https://github.com/stackabletech/nifi-operator/pull/510
        // As a workaround, reencrypt the keystore with keytool.
        // keytool crashes if the target truststore already exists (covering up the true error
        // if the init container fails later on in the script), so delete it first.
        format!("test ! -e {STACKABLE_SERVER_TLS_DIR}/truststore.p12 || rm {STACKABLE_SERVER_TLS_DIR}/truststore.p12"),
        format!("keytool -importkeystore -srckeystore {KEYSTORE_NIFI_CONTAINER_MOUNT}/truststore.p12 -destkeystore {STACKABLE_SERVER_TLS_DIR}/truststore.p12 -srcstorepass {STACKABLE_TLS_STORE_PASSWORD} -deststorepass {STACKABLE_TLS_STORE_PASSWORD}"),

        "echo Replacing config directory".to_string(),
        "cp /conf/* /stackable/nifi/conf".to_string(),
        "test -L /stackable/nifi/conf/logback.xml || ln -sf /stackable/log_config/logback.xml /stackable/nifi/conf/logback.xml".to_string(),
        format!(r#"export NODE_ADDRESS="{node_address}""#),
    ]);

    // This commands needs to go first, as they might set env variables needed by the templating
    prepare_args.extend_from_slice(
        authentication_config
            .get_additional_container_args()
            .as_slice(),
    );

    // Add OPA certificate to truststore if OPA TLS is enabled
    if authorization_config.has_opa_tls() {
        prepare_args.extend(vec![
            "echo Importing OPA CA certificate to truststore".to_string(),
            format!("keytool -importcert -file {OPA_TLS_MOUNT_PATH}/ca.crt -keystore {STACKABLE_SERVER_TLS_DIR}/truststore.p12 -storepass {STACKABLE_TLS_STORE_PASSWORD} -alias opa-ca -noprompt"),
        ]);
    }

    prepare_args.extend(vec![
        "export LISTENER_DEFAULT_ADDRESS=$(cat /stackable/listener/default-address/address)"
            .to_string(),
    ]);
    prepare_args.extend(vec![
        "export LISTENER_DEFAULT_PORT_HTTPS=$(cat /stackable/listener/default-address/ports/https)"
            .to_string(),
    ]);

    prepare_args.extend(vec![
        "echo Templating config files".to_string(),
        "config-utils template /stackable/nifi/conf/nifi.properties".to_string(),
        "config-utils template /stackable/nifi/conf/state-management.xml".to_string(),
        "config-utils template /stackable/nifi/conf/login-identity-providers.xml".to_string(),
        "config-utils template /stackable/nifi/conf/authorizers.xml".to_string(),
        "config-utils template /stackable/nifi/conf/security.properties".to_string(),
    ]);

    let mut container_prepare =
        ContainerBuilder::new(&prepare_container_name).with_context(|_| {
            IllegalContainerNameSnafu {
                container_name: prepare_container_name.to_string(),
            }
        })?;

    container_prepare
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-c".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
        ])
        .add_env_vars(env_vars.clone())
        .args(vec![prepare_args.join(" && ")])
        .add_volume_mounts(
            PERSISTENT_REPOSITORIES
                .iter()
                .map(NifiRepository::volume_mount),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(CONFIG_VOLUME_NAME, CONFIG_VOLUME_MOUNT)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(ACTIVE_CONFIG_VOLUME_NAME, NIFI_CONFIG_DIRECTORY)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(
            SENSITIVE_PROPERTY_VOLUME_NAME,
            SENSITIVE_PROPERTY_VOLUME_MOUNT,
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(TRUSTSTORE_VOLUME_NAME, STACKABLE_SERVER_TLS_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LISTENER_VOLUME_NAME, LISTENER_VOLUME_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mounts(authorization_config.get_volume_mounts())
        .context(AddVolumeMountSnafu)?
        .resources(
            ResourceRequirementsBuilder::new()
                .with_cpu_request("500m")
                .with_cpu_limit("2000m")
                .with_memory_request("4096Mi")
                .with_memory_limit("4096Mi")
                .build(),
        );

    let nifi_container_name = Container::Nifi.to_string();
    let mut container_nifi_builder =
        ContainerBuilder::new(&nifi_container_name).with_context(|_| {
            IllegalContainerNameSnafu {
                container_name: nifi_container_name,
            }
        })?;

    let nifi_args = vec![formatdoc! {"
            {COMMON_BASH_TRAP_FUNCTIONS}
            {remove_vector_shutdown_file_command}
            prepare_signal_handlers
            containerdebug --output={STACKABLE_LOG_DIR}/containerdebug-state.json --loop &
            bin/nifi.sh run &
            wait_for_termination $!
            {create_vector_shutdown_file_command}
            ",
    remove_vector_shutdown_file_command =
        remove_vector_shutdown_file_command(STACKABLE_LOG_DIR),
    create_vector_shutdown_file_command =
        create_vector_shutdown_file_command(STACKABLE_LOG_DIR),
    }];

    let container_nifi = container_nifi_builder
        .image_from_product_image(resolved_product_image)
        .command(vec![
            "/bin/bash".to_string(),
            "-x".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
        ])
        .args(nifi_args)
        .add_env_vars(env_vars)
        .add_volume_mount(KEYSTORE_VOLUME_NAME, KEYSTORE_NIFI_CONTAINER_MOUNT)
        .context(AddVolumeMountSnafu)?
        .add_volume_mounts(
            PERSISTENT_REPOSITORIES
                .iter()
                .map(NifiRepository::volume_mount),
        )
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(ACTIVE_CONFIG_VOLUME_NAME, NIFI_CONFIG_DIRECTORY)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_CONFIG_VOLUME_NAME, STACKABLE_LOG_CONFIG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LOG_VOLUME_NAME, STACKABLE_LOG_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(TRUSTSTORE_VOLUME_NAME, STACKABLE_SERVER_TLS_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mount(LISTENER_VOLUME_NAME, LISTENER_VOLUME_DIR)
        .context(AddVolumeMountSnafu)?
        .add_volume_mounts(authorization_config.get_volume_mounts())
        .context(AddVolumeMountSnafu)?
        .add_container_port(HTTPS_PORT_NAME, HTTPS_PORT.into())
        .add_container_port(PROTOCOL_PORT_NAME, PROTOCOL_PORT.into())
        .add_container_port(BALANCE_PORT_NAME, BALANCE_PORT.into())
        .liveness_probe(Probe {
            initial_delay_seconds: Some(10),
            period_seconds: Some(10),
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
                ..TCPSocketAction::default()
            }),
            ..Probe::default()
        })
        .startup_probe(Probe {
            initial_delay_seconds: Some(10),
            period_seconds: Some(10),
            failure_threshold: Some(20 * 6),
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
                ..TCPSocketAction::default()
            }),
            ..Probe::default()
        })
        .resources(merged_config.resources.clone().into());

    // NiFi 2.x.x offers nifi-api/flow/metrics/prometheus at the HTTPS_PORT, therefore METRICS_PORT is only required for NiFi 1.x.x.
    if resolved_product_image.product_version.starts_with("1.") {
        container_nifi.add_container_port(METRICS_PORT_NAME, METRICS_PORT.into());
    }

    let mut pod_builder = PodBuilder::new();

    let recommended_object_labels = cluster.recommended_labels(role_group_name);

    add_graceful_shutdown_config(merged_config, &mut pod_builder).context(GracefulShutdownSnafu)?;

    // Add user configured extra volumes if any are specified
    for volume in &nifi.spec.cluster_config.extra_volumes {
        // Extract values into vars so we make it impossible to log something other than
        // what we actually use to create the mounts - maybe paranoid, but hey ..
        let volume_name = &volume.name;
        let mount_point = format!("{USERDATA_MOUNTPOINT}/{}", volume.name);

        tracing::info!(
            ?volume_name,
            ?mount_point,
            ?role,
            "Adding user specified extra volume",
        );
        pod_builder
            .add_volume(volume.clone())
            .context(AddVolumeSnafu)?;
        container_nifi
            .add_volume_mount(volume_name, mount_point)
            .context(AddVolumeMountSnafu)?;
    }

    let volume_name = "nifi-python-working-directory".to_string();
    pod_builder
        .add_empty_dir_volume(&volume_name, None)
        .context(AddVolumeSnafu)?;
    container_nifi
        .add_volume_mount(&volume_name, NIFI_PYTHON_WORKING_DIRECTORY)
        .context(AddVolumeMountSnafu)?;

    container_nifi
        .add_volume_mounts(git_sync_resources.git_content_volume_mounts.to_owned())
        .context(AddVolumeMountSnafu)?;

    // We want to add nifi container first for easier defaulting into this container
    pod_builder.add_container(container_nifi.build());
    // After calling `build()` the ContainerBuilder shouldn't be used anymore, so we drop it
    drop(container_nifi_builder);

    for container in git_sync_resources.git_sync_containers.iter().cloned() {
        pod_builder.add_container(container);
    }
    for container in git_sync_resources.git_sync_init_containers.iter().cloned() {
        pod_builder.add_init_container(container);
    }
    pod_builder
        .add_volumes(git_sync_resources.git_content_volumes.to_owned())
        .context(AddVolumeSnafu)?;
    pod_builder
        .add_volumes(git_sync_resources.git_ca_cert_volumes.to_owned())
        .context(AddVolumeSnafu)?;

    if let Some(ContainerLogConfig {
        choice:
            Some(ContainerLogConfigChoice::Custom(CustomContainerLogConfig {
                custom: ConfigMapLogConfig { config_map },
            })),
    }) = merged_config.logging.containers.get(&Container::Nifi)
    {
        pod_builder
            .add_volume(Volume {
                name: LOG_CONFIG_VOLUME_NAME.to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: config_map.clone(),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            })
            .context(AddVolumeSnafu)?;
    } else {
        pod_builder
            .add_volume(Volume {
                name: LOG_CONFIG_VOLUME_NAME.to_string(),
                config_map: Some(ConfigMapVolumeSource {
                    name: resource_names.role_group_config_map().to_string(),
                    ..ConfigMapVolumeSource::default()
                }),
                ..Volume::default()
            })
            .context(AddVolumeSnafu)?;
    }

    if merged_config.logging.enable_vector_agent {
        match &nifi.spec.cluster_config.vector_aggregator_config_map_name {
            Some(vector_aggregator_config_map_name) => {
                pod_builder.add_container(
                    product_logging::framework::vector_container(
                        resolved_product_image,
                        "config",
                        LOG_VOLUME_NAME,
                        merged_config.logging.containers.get(&Container::Vector),
                        ResourceRequirementsBuilder::new()
                            .with_cpu_request("250m")
                            .with_cpu_limit("500m")
                            .with_memory_request("128Mi")
                            .with_memory_limit("128Mi")
                            .build(),
                        vector_aggregator_config_map_name,
                    )
                    .context(ConfigureLoggingSnafu)?,
                );
            }
            None => {
                VectorAggregatorConfigMapMissingSnafu.fail()?;
            }
        }
    }

    authentication_config
        .add_volumes_and_mounts(&mut pod_builder, vec![&mut container_prepare])
        .context(AddAuthVolumesSnafu)?;

    let metadata = ObjectMetaBuilder::new()
        .with_labels(recommended_object_labels.clone())
        .build();

    let requested_secret_lifetime = merged_config
        .requested_secret_lifetime
        .context(MissingSecretLifetimeSnafu)?;
    let nifi_cluster_name = nifi.name_any();
    pod_builder
        .metadata(metadata)
        .image_pull_secrets_from_product_image(resolved_product_image)
        .add_init_container(container_prepare.build())
        .affinity(&merged_config.affinity)
        // One volume for the NiFi configuration. A script will later on edit (e.g. nodename)
        // and copy the whole content to the <NIFI_HOME>/conf folder.
        .add_volume(stackable_operator::k8s_openapi::api::core::v1::Volume {
            name: "config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: resource_names.role_group_config_map().to_string(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .context(AddVolumeSnafu)?
        .add_volume(Volume {
            name: CONFIG_VOLUME_NAME.to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: resource_names.role_group_config_map().to_string(),
                ..ConfigMapVolumeSource::default()
            }),
            ..Volume::default()
        })
        .context(AddVolumeSnafu)?
        .add_empty_dir_volume(
            LOG_VOLUME_NAME,
            // Set volume size to higher than theoretically necessary to avoid running out of disk space as log rotation triggers are only checked by Logback every 5s.
            Some(
                MemoryQuantity {
                    value: 500.0,
                    unit: BinaryMultiple::Mebi,
                }
                .into(),
            ),
        )
        .context(AddVolumeSnafu)?
        // One volume for the keystore and truststore data configmap
        .add_volume(
            build_tls_volume(
                nifi,
                KEYSTORE_VOLUME_NAME,
                [
                    crate::controller::build::resource::service::metrics_service_name(
                        cluster,
                        role_group_name,
                    ),
                    build_reporting_task_service_name(&nifi_cluster_name),
                ],
                SecretFormat::TlsPkcs12,
                &requested_secret_lifetime,
                Some(LISTENER_VOLUME_NAME),
            )
            .context(SecuritySnafu)?,
        )
        .context(AddVolumeSnafu)?
        .add_empty_dir_volume(TRUSTSTORE_VOLUME_NAME, None)
        .context(AddVolumeSnafu)?
        .add_volumes(
            authorization_config
                .get_volumes()
                .context(AuthorizationConfigurationSnafu)?,
        )
        .context(AddVolumeSnafu)?;

    pod_builder
        .add_volume(Volume {
            name: SENSITIVE_PROPERTY_VOLUME_NAME.to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(sensitive_key_secret.to_string()),
                ..SecretVolumeSource::default()
            }),
            ..Volume::default()
        })
        .context(AddVolumeSnafu)?
        .add_volume(Volume {
            empty_dir: Some(EmptyDirVolumeSource {
                medium: None,
                size_limit: None,
            }),
            name: ACTIVE_CONFIG_VOLUME_NAME.to_string(),
            ..Volume::default()
        })
        .context(AddVolumeSnafu)?
        .service_account_name(service_account_name)
        .security_context(PodSecurityContextBuilder::new().fs_group(1000).build());

    let mut labels = BTreeMap::new();
    labels.insert(
        "app.kubernetes.io/instance".to_string(),
        nifi.metadata
            .name
            .as_deref()
            .with_context(|| ObjectHasNoNameSnafu {})?
            .to_string(),
    );

    let mut pod_template = pod_builder.build_template();
    // `rg.pod_overrides` is already the role <- rolegroup merge produced by the framework.
    pod_template.merge_from(rg.pod_overrides.clone());

    Ok(StatefulSet {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(cluster)
            .name(resource_names.stateful_set_name().to_string())
            .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
            .with_labels(recommended_object_labels)
            .with_label(RESTART_CONTROLLER_ENABLED_LABEL.to_owned())
            .build(),
        spec: Some(StatefulSetSpec {
            pod_management_policy: Some("Parallel".to_string()),
            replicas,
            selector: LabelSelector {
                match_labels: Some(cluster.role_group_selector(role_group_name).into()),
                ..LabelSelector::default()
            },
            service_name: Some(resource_names.headless_service_name().to_string()),
            template: pod_template,
            update_strategy: Some(StatefulSetUpdateStrategy {
                type_: if rolling_update_supported {
                    Some("RollingUpdate".to_string())
                } else {
                    Some("OnDelete".to_string())
                },
                ..StatefulSetUpdateStrategy::default()
            }),
            volume_claim_templates: Some(get_volume_claim_templates(
                nifi,
                cluster,
                role_group_name,
                merged_config,
                authorization_config,
            )?),
            ..StatefulSetSpec::default()
        }),
        status: None,
    })
}

fn get_volume_claim_templates(
    nifi: &v1alpha1::NifiCluster,
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
    merged_config: &NifiConfig,
    authorization_config: &ResolvedNifiAuthorizationConfig,
) -> Result<Vec<PersistentVolumeClaim>> {
    let mut pvcs = vec![
        merged_config.resources.storage.content_repo.build_pvc(
            &NifiRepository::Content.repository(),
            Some(vec!["ReadWriteOnce"]),
        ),
        merged_config.resources.storage.database_repo.build_pvc(
            &NifiRepository::Database.repository(),
            Some(vec!["ReadWriteOnce"]),
        ),
        merged_config.resources.storage.flowfile_repo.build_pvc(
            &NifiRepository::Flowfile.repository(),
            Some(vec!["ReadWriteOnce"]),
        ),
        merged_config.resources.storage.provenance_repo.build_pvc(
            &NifiRepository::Provenance.repository(),
            Some(vec!["ReadWriteOnce"]),
        ),
        merged_config.resources.storage.state_repo.build_pvc(
            &NifiRepository::State.repository(),
            Some(vec!["ReadWriteOnce"]),
        ),
    ];

    // Used for PVC templates that cannot be modified once they are deployed, so the version label
    // is set to the placeholder `none` to keep the labels stable across version upgrades.
    let unversioned_recommended_labels = cluster.recommended_labels_unversioned(role_group_name);

    // listener endpoints will use persistent volumes
    // so that load balancers can hard-code the target addresses and
    // that it is possible to connect to a consistent address
    pvcs.push(
        build_group_listener_pvc(
            &group_listener_name(nifi, &NifiRole::Node.to_string()),
            &unversioned_recommended_labels,
        )
        .context(ListenerConfigurationSnafu)?,
    );

    // Add file-based PVC if required
    if let ResolvedNifiAuthorizationConfig::Standard {
        access_policy_provider: NifiAccessPolicyProvider::FileBased { .. },
    } = authorization_config
    {
        pvcs.push(merged_config.resources.storage.filebased_repo.build_pvc(
            &NifiRepository::Filebased.repository(),
            Some(vec!["ReadWriteOnce"]),
        ))
    }

    Ok(pvcs)
}
