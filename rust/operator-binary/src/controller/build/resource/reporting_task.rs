//! The NiFi Reporting Task for Prometheus metrics is created via the NiFi Rest API.
//!
//! This module contains methods to create all required resources (Job, Service) and helper methods
//! to create the Prometheus Reporting Task.
//!
//! Before NiFi 1.25.0 there was only the actual Kubernetes Job required to create and run the Reporting Task Job.
//!
//! Due to changes in the JWT validation in 1.25.0, the issuer refers to the FQDN of the Pod that was created, e.g.:
//! {
//!     "sub": "admin",
//!     "iss": "test-nifi-node-default-0.test-nifi-node-default-headless.default.svc.cluster.local:8443",
//! }
//! which was different in e.g. 1.23.2
//! {
//!     "sub": "admin",
//!     "iss": "SingleUserLoginIdentityProvider",
//! }
//! This caused problems when using the generated JWT against a different node (due to randomness of the service).
//!
//! "An error occurred while attempting to decode the Jwt: Signed JWT rejected: Another algorithm expected, or no matching key(s) found"
//!
//! Therefore, since the support of NiFi 1.25.0, an additional service for the Reporting Task Job containing a
//! random but deterministic NiFi node to ensure the communication with a single node.
//!
use std::{collections::BTreeMap, str::FromStr as _};

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{
        self,
        meta::ObjectMetaBuilder,
        pod::{
            PodBuilder, resources::ResourceRequirementsBuilder,
            security::PodSecurityContextBuilder, volume::SecretFormat,
        },
    },
    k8s_openapi::{
        DeepMerge,
        api::{
            batch::v1::{Job, JobSpec},
            core::v1::{Service, ServicePort, ServiceSpec},
        },
    },
    shared::time::Duration,
    utils::cluster_info::KubernetesClusterInfo,
    v2::{
        builder::{meta::ownerreference_from_resource, pod::container::new_container_builder},
        types::{
            kubernetes::{ContainerName, NamespaceName, VolumeName},
            operator::RoleGroupName,
        },
    },
};

use crate::{
    controller::{
        ValidatedCluster,
        build::{HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT},
    },
    crd::NifiRole,
    security::{authentication::STACKABLE_ADMIN_USERNAME, build_tls_volume},
};

stackable_operator::constant!(REPORTING_TASK_CERT_VOLUME_NAME: VolumeName = "tls");
const REPORTING_TASK_CERT_VOLUME_MOUNT: &str = "/stackable/cert";
/// Path (inside the image) of the script that registers the NiFi 1.x reporting task.
const REPORTING_TASK_SCRIPT_PATH: &str = "/stackable/python/create_nifi_reporting_task.py";
stackable_operator::constant!(REPORTING_TASK_CONTAINER_NAME: ContainerName = "reporting-task");

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to add Authentication Volumes and VolumeMounts"))]
    AddAuthVolumes {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("failed to build secret volume"))]
    SecretVolumeBuildFailure { source: crate::security::Error },

    #[snafu(display("failed to create reporting task service, no role groups defined"))]
    FailedBuildReportingTaskService,

    #[snafu(display("failed to add needed volume"))]
    AddVolume { source: builder::pod::Error },

    #[snafu(display("failed to add needed volumeMount"))]
    AddVolumeMount {
        source: builder::pod::container::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Build required resources to create the reporting task in NiFi versions 1.x.
///
/// This will return
/// * a Job that creates and runs the reporting task via the NiFi Rest API.
/// * a Service that contains of one single NiFi node.
///
/// The Service is required in order to communicate only with one designated NiFi node.
/// This is necessary as the generated JWT was changed in 1.25.0 and corrected the issuer
/// from SingleUserLoginIdentityProvider to the FQDN of the pod.
/// The NiFi role service will randomly delegate to different NiFi nodes which will
/// then fail requests to other nodes.
///
/// NiFi 2.x and above automatically server Prometheus metrics via the API, but as of 2024-11-08
/// requires authentication.
pub fn build_maybe_reporting_task(
    cluster: &ValidatedCluster,
    cluster_info: &KubernetesClusterInfo,
    sa_name: &str,
) -> Result<Option<(Job, Service)>> {
    if cluster.image.product_version.starts_with("1.") {
        Ok(Some((
            build_reporting_task_job(cluster, cluster_info, sa_name)?,
            build_reporting_task_service(cluster)?,
        )))
    } else {
        Ok(None)
    }
}

/// The placeholder role-group name (`global`) used for the labels of the cluster-global reporting
/// task resources, which are not tied to a specific role group.
fn reporting_task_role_group() -> RoleGroupName {
    RoleGroupName::from_str("global").expect("'global' is a valid role-group name")
}

/// Return the name of the reporting task Service.
pub fn build_reporting_task_service_name(nifi_cluster_name: &str) -> String {
    format!(
        "{nifi_cluster_name}-{container}",
        container = &*REPORTING_TASK_CONTAINER_NAME
    )
}

/// Return the FQDN (with namespace, domain) of the reporting task.
pub fn build_reporting_task_fqdn_service_name(
    cluster_name: &str,
    namespace: &NamespaceName,
    cluster_info: &KubernetesClusterInfo,
) -> String {
    let reporting_task_service_name = build_reporting_task_service_name(cluster_name);
    let cluster_domain = &cluster_info.cluster_domain;
    format!("{reporting_task_service_name}.{namespace}.svc.{cluster_domain}")
}

/// Return the name of the first pod belonging to the first role group that contains more than 0 replicas.
/// If no role group has replicas set (e.g. HPA, see <https://docs.stackable.tech/home/stable/concepts/operations/#_performance>)
/// return the first rolegroup just in case.
/// This is required to only select a single node in the Reporting Task Service.
fn get_reporting_task_service_selector_pod(cluster: &ValidatedCluster) -> Result<String> {
    let node_name = NifiRole::Node.to_string();

    // The role groups are already sorted by name (`BTreeMap`), avoiding random ordering and
    // therefore unnecessary reconciles.
    let role_groups = cluster
        .role_group_configs
        .get(&NifiRole::Node)
        .context(FailedBuildReportingTaskServiceSnafu)?;

    let mut selector_role_group = None;
    for (role_group_name, role_group) in role_groups {
        // just pick the first rolegroup in case none has replicas set
        if selector_role_group.is_none() {
            selector_role_group = Some(role_group_name);
        }

        if role_group.replicas.unwrap_or(1) > 0 {
            selector_role_group = Some(role_group_name);
            break;
        }
    }

    Ok(format!(
        "{cluster_name}-{node_name}-{role_group_name}-0",
        cluster_name = cluster.name,
        role_group_name = selector_role_group.context(FailedBuildReportingTaskServiceSnafu)?
    ))
}

/// Build the internal Reporting Task Service in order to communicate with a single NiFi node.
fn build_reporting_task_service(cluster: &ValidatedCluster) -> Result<Service> {
    let nifi_cluster_name = cluster.name.to_string();
    let mut selector: BTreeMap<String, String> = cluster.role_selector().into();

    let service_selector_pod = get_reporting_task_service_selector_pod(cluster)?;
    selector.insert(
        "statefulset.kubernetes.io/pod-name".to_string(),
        service_selector_pod,
    );

    Ok(Service {
        metadata: cluster
            .object_meta(
                build_reporting_task_service_name(&nifi_cluster_name),
                &reporting_task_role_group(),
            )
            .build(),
        spec: Some(ServiceSpec {
            ports: Some(vec![ServicePort {
                name: Some(HTTPS_PORT_NAME.to_string()),
                port: HTTPS_PORT.into(),
                protocol: Some("TCP".to_string()),
                ..ServicePort::default()
            }]),
            selector: Some(selector),
            ..ServiceSpec::default()
        }),
        status: None,
    })
}

/// Build the [`Job`](`stackable_operator::k8s_openapi::api::batch::v1::Job`) that creates a
/// NiFi `ReportingTask` in order to enable JVM and NiFi metrics.
///
/// The Job is run via the [`tools`](https://github.com/stackabletech/docker-images/tree/main/tools)
/// docker image and more specifically the `create_nifi_reporting_task.py` Python script.
///
/// This script uses the [`nipyapi`](https://nipyapi.readthedocs.io/en/latest/readme.html)
/// library to authenticate and run the required REST calls to the NiFi REST API.
///
/// In order to authenticate we need the `username` and `password` from the
/// [`NifiAuthenticationConfig`](`crate::security::authentication::NifiAuthenticationConfig`)
/// as well as a public certificate provided by the Stackable
/// [`secret-operator`](https://github.com/stackabletech/secret-operator)
///
fn build_reporting_task_job(
    cluster: &ValidatedCluster,
    cluster_info: &KubernetesClusterInfo,
    sa_name: &str,
) -> Result<Job> {
    let resolved_product_image = &cluster.image;
    let nifi_auth_config = &cluster.cluster_config.authentication;
    let reporting_task_fqdn_service_name = build_reporting_task_fqdn_service_name(
        cluster.name.as_ref(),
        &cluster.namespace,
        cluster_info,
    );
    let product_version = &resolved_product_image.product_version;
    let nifi_connect_url =
        format!("https://{reporting_task_fqdn_service_name}:{HTTPS_PORT}/nifi-api",);

    let (admin_username_file, admin_password_file) =
        nifi_auth_config.get_user_and_password_file_paths();

    let user_name_command = if admin_username_file.is_empty() {
        // In case of the username being simple (e.g admin for SingleUser) just use it as is
        format!("-u {STACKABLE_ADMIN_USERNAME}")
    } else {
        // If the username is a bind dn (e.g. cn=integrationtest,ou=my users,dc=example,dc=org) we have to extract the cn/dn/uid (in this case integrationtest)
        format!(
            "-u \"$(cat {admin_username_file} | grep -oP '((cn|dn|uid)=\\K[^,]+|.*)' | head -n 1)\""
        )
    };

    let args = [
        REPORTING_TASK_SCRIPT_PATH.to_string(),
        format!("-n {nifi_connect_url}"),
        user_name_command,
        format!("-p \"$(cat {admin_password_file})\""),
        format!("-m {METRICS_PORT}"),
        format!("-c {REPORTING_TASK_CERT_VOLUME_MOUNT}/ca.crt"),
    ];
    let mut cb = new_container_builder(&REPORTING_TASK_CONTAINER_NAME);
    cb.image_from_product_image(resolved_product_image)
        .command(vec!["sh".to_string(), "-c".to_string()])
        .args(vec![args.join(" ")])
        // The VolumeMount for the secret operator key store certificates
        .add_volume_mount(
            REPORTING_TASK_CERT_VOLUME_NAME.to_string(),
            REPORTING_TASK_CERT_VOLUME_MOUNT,
        )
        .context(AddVolumeMountSnafu)?
        .resources(
            ResourceRequirementsBuilder::new()
                .with_cpu_request("100m")
                .with_cpu_limit("400m")
                .with_memory_request("512Mi")
                .with_memory_limit("512Mi")
                .build(),
        );

    let job_name = format!(
        "{}-create-reporting-task-{}",
        cluster.name,
        product_version.replace('.', "-").to_ascii_lowercase()
    );

    let mut pb = PodBuilder::new();
    nifi_auth_config
        .add_volumes_and_mounts(&mut pb, vec![&mut cb])
        .context(AddAuthVolumesSnafu)?;

    let mut pod_template = pb
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(cluster)
                .name(job_name.clone())
                .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
                .build(),
        )
        .image_pull_secrets_from_product_image(resolved_product_image)
        .restart_policy("OnFailure")
        .service_account_name(sa_name)
        .security_context(PodSecurityContextBuilder::new().fs_group(1000).build())
        .add_container(cb.build())
        .add_volume(
            build_tls_volume(
                &cluster.cluster_config.server_tls_secret_class,
                &REPORTING_TASK_CERT_VOLUME_NAME,
                Vec::<String>::new(),
                SecretFormat::TlsPem,
                // The certificate is only used for the REST API call, so a short lifetime is sufficient.
                // There is no correct way to configure this job since it's an implementation detail.
                // Also it will be dropped when support for 1.x is removed.
                &Duration::from_days_unchecked(1),
                // There is no listener volume we could get certs for
                None,
            )
            .context(SecretVolumeBuildFailureSnafu)?,
        )
        .context(AddVolumeSnafu)?
        .build_template();

    pod_template.merge_from(cluster.cluster_config.reporting_task.pod_overrides.clone());

    let job = Job {
        metadata: cluster
            .object_meta(job_name, &reporting_task_role_group())
            .build(),
        spec: Some(JobSpec {
            backoff_limit: Some(100),
            ttl_seconds_after_finished: Some(120),
            template: pod_template,
            ..JobSpec::default()
        }),
        ..Job::default()
    };

    Ok(job)
}
