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
//!     "iss": "test-nifi-node-default-0.test-nifi-node-default.default.svc.cluster.local:8443",
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
use std::collections::BTreeMap;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_nifi_crd::{
    NifiCluster, NifiRole, APP_NAME, HTTPS_PORT, HTTPS_PORT_NAME, METRICS_PORT,
};
use stackable_operator::{
    builder::{
        meta::ObjectMetaBuilder,
        pod::{
            container::ContainerBuilder, resources::ResourceRequirementsBuilder,
            security::PodSecurityContextBuilder, volume::SecretFormat, PodBuilder,
        },
    },
    commons::product_image_selection::ResolvedProductImage,
    k8s_openapi::api::{
        batch::v1::{Job, JobSpec},
        core::v1::{Service, ServicePort, ServiceSpec},
    },
    kube::ResourceExt,
    kvp::Labels,
};

use crate::security::{
    authentication::{NifiAuthenticationConfig, STACKABLE_ADMIN_USER_NAME},
    build_tls_volume,
};

use super::controller::{build_recommended_labels, NIFI_UID};

const REPORTING_TASK_CERT_VOLUME_NAME: &str = "tls";
const REPORTING_TASK_CERT_VOLUME_MOUNT: &str = "/stackable/cert";
const REPORTING_TASK_CONTAINER_NAME: &str = "reporting-task";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object defines no name"))]
    ObjectHasNoName,

    #[snafu(display("object defines no namespace"))]
    ObjectHasNoNamespace,

    #[snafu(display("failed to build metadata"))]
    MetadataBuild {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("illegal container name: [{container_name}]"))]
    IllegalContainerName {
        source: stackable_operator::builder::pod::container::Error,
        container_name: String,
    },

    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to add Authentication Volumes and VolumeMounts"))]
    AddAuthVolumes {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("failed to build labels"))]
    LabelBuild {
        source: stackable_operator::kvp::LabelError,
    },

    #[snafu(display("failed to build secret volume"))]
    SecretVolumeBuildFailure { source: crate::security::Error },

    #[snafu(display("failed to create reporting task service, no role groups defined"))]
    FailedBuildReportingTaskService,
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Build required resources to create the reporting task in NiFi.
/// This will return
/// * a Job that creates and runs the reporting task via the NiFi Rest API.
/// * a Service that contains of one single NiFi node.
///
/// The Service is required in order to communicate only with one designated NiFi node.
/// This is necessary as the generated JWT was changed in 1.25.0 and corrected the issuer
/// from SingleUserLoginIdentityProvider to the FQDN of the pod.
/// The NiFi role service will randomly delegate to different NiFi nodes which will
/// then fail requests to other nodes.
pub fn build_reporting_task(
    nifi: &NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    nifi_auth_config: &NifiAuthenticationConfig,
    sa_name: &str,
) -> Result<(Job, Service)> {
    Ok((
        build_reporting_task_job(nifi, resolved_product_image, nifi_auth_config, sa_name)?,
        build_reporting_task_service(nifi, resolved_product_image)?,
    ))
}

/// Return the name of the reporting task.
pub fn build_reporting_task_service_name(nifi_cluster_name: &str) -> String {
    format!("{nifi_cluster_name}-{REPORTING_TASK_CONTAINER_NAME}")
}

/// Return the FQDN (with namespace, domain) of the reporting task.
pub fn build_reporting_task_fqdn_service_name(nifi: &NifiCluster) -> Result<String> {
    let nifi_cluster_name = nifi.name_any();
    let nifi_namespace: &str = &nifi.namespace().context(ObjectHasNoNamespaceSnafu)?;
    let reporting_task_service_name = build_reporting_task_service_name(&nifi_cluster_name);

    Ok(format!(
        "{reporting_task_service_name}.{nifi_namespace}.svc.cluster.local"
    ))
}

/// Return the name of the first pod belonging to the first role group that contains more than 0 replicas.
/// If no replicas are set in any rolegroup (e.g. HPA, see <https://docs.stackable.tech/home/stable/concepts/operations/#_performance>)
/// return the first rolegroup just in case.
/// This is required to only select a single node in the Reporting Task Service.
fn get_reporting_task_service_selector_pod(nifi: &NifiCluster) -> Result<String> {
    let cluster_name = nifi.name_any();
    let node_name = NifiRole::Node.to_string();

    // sort the rolegroups to avoid random sorting and therefore unnecessary reconciles
    let sorted_role_groups = nifi
        .spec
        .nodes
        .iter()
        .flat_map(|role| &role.role_groups)
        .collect::<BTreeMap<_, _>>();

    let mut selector_role_group = None;
    for (role_group_name, role_group) in sorted_role_groups {
        // just pick the first rolegroup in case no replicas are set
        if selector_role_group.is_none() {
            selector_role_group = Some(role_group_name);
        }

        if let Some(replicas) = role_group.replicas {
            if replicas > 0 {
                selector_role_group = Some(role_group_name);
                break;
            }
        }
    }

    Ok(format!(
        "{cluster_name}-{node_name}-{role_group_name}-0",
        role_group_name = selector_role_group.context(FailedBuildReportingTaskServiceSnafu)?
    ))
}

/// Build the internal Reporting Task Service in order to communicate with a single NiFi node.
fn build_reporting_task_service(
    nifi: &NifiCluster,
    resolved_product_image: &ResolvedProductImage,
) -> Result<Service> {
    let nifi_cluster_name = nifi.name_any();
    let role_name = NifiRole::Node.to_string();
    let mut selector: BTreeMap<String, String> = Labels::role_selector(nifi, APP_NAME, &role_name)
        .context(LabelBuildSnafu)?
        .into();

    let service_selector_pod = get_reporting_task_service_selector_pod(nifi)?;
    selector.insert(
        "statefulset.kubernetes.io/pod-name".to_string(),
        service_selector_pod,
    );

    Ok(Service {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(build_reporting_task_service_name(&nifi_cluster_name))
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                &role_name,
                "global",
            ))
            .context(MetadataBuildSnafu)?
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
    nifi: &NifiCluster,
    resolved_product_image: &ResolvedProductImage,
    nifi_auth_config: &NifiAuthenticationConfig,
    sa_name: &str,
) -> Result<Job> {
    let reporting_task_fqdn_service_name = build_reporting_task_fqdn_service_name(nifi)?;
    let product_version = &resolved_product_image.product_version;
    let nifi_connect_url =
        format!("https://{reporting_task_fqdn_service_name}:{HTTPS_PORT}/nifi-api",);

    let (admin_username_file, admin_password_file) =
        nifi_auth_config.get_user_and_password_file_paths();

    let user_name_command = if admin_username_file.is_empty() {
        // In case of the username being simple (e.g admin for SingleUser) just use it as is
        format!("-u {STACKABLE_ADMIN_USER_NAME}")
    } else {
        // If the username is a bind dn (e.g. cn=integrationtest,ou=my users,dc=example,dc=org) we have to extract the cn/dn/uid (in this case integrationtest)
        format!(
            "-u \"$(cat {admin_username_file} | grep -oP '((cn|dn|uid)=\\K[^,]+|.*)' | head -n 1)\""
        )
    };

    let args = [
        "/stackable/python/create_nifi_reporting_task.py".to_string(),
        format!("-n {nifi_connect_url}"),
        user_name_command,
        format!("-p \"$(cat {admin_password_file})\""),
        format!("-v {product_version}"),
        format!("-m {METRICS_PORT}"),
        format!("-c {REPORTING_TASK_CERT_VOLUME_MOUNT}/ca.crt"),
    ];
    let mut cb = ContainerBuilder::new(REPORTING_TASK_CONTAINER_NAME).with_context(|_| {
        IllegalContainerNameSnafu {
            container_name: REPORTING_TASK_CONTAINER_NAME.to_string(),
        }
    })?;
    cb.image_from_product_image(resolved_product_image)
        .command(vec!["sh".to_string(), "-c".to_string()])
        .args(vec![args.join(" ")])
        // The VolumeMount for the secret operator key store certificates
        .add_volume_mount(
            REPORTING_TASK_CERT_VOLUME_NAME,
            REPORTING_TASK_CERT_VOLUME_MOUNT,
        )
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
        nifi.name_any(),
        product_version.replace('.', "-")
    );

    let mut pb = PodBuilder::new();
    nifi_auth_config
        .add_volumes_and_mounts(&mut pb, vec![&mut cb])
        .context(AddAuthVolumesSnafu)?;

    let pod = pb
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(nifi)
                .name(job_name.clone())
                .ownerreference_from_resource(nifi, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRefSnafu)?
                .build(),
        )
        .image_pull_secrets_from_product_image(resolved_product_image)
        .restart_policy("OnFailure")
        .service_account_name(sa_name)
        .security_context(
            PodSecurityContextBuilder::new()
                .run_as_user(NIFI_UID)
                .run_as_group(0)
                .fs_group(1000)
                .build(),
        )
        .add_container(cb.build())
        .add_volume(
            build_tls_volume(
                REPORTING_TASK_CERT_VOLUME_NAME,
                vec![],
                SecretFormat::TlsPem,
            )
            .context(SecretVolumeBuildFailureSnafu)?,
        )
        .build_template();

    let job = Job {
        metadata: ObjectMetaBuilder::new()
            .name_and_namespace(nifi)
            .name(job_name)
            .ownerreference_from_resource(nifi, None, Some(true))
            .context(ObjectMissingMetadataForOwnerRefSnafu)?
            .with_recommended_labels(build_recommended_labels(
                nifi,
                &resolved_product_image.app_version_label,
                "global",
                "global",
            ))
            .context(MetadataBuildSnafu)?
            .build(),
        spec: Some(JobSpec {
            backoff_limit: Some(100),
            ttl_seconds_after_finished: Some(120),
            template: pod,
            ..JobSpec::default()
        }),
        ..Job::default()
    };

    Ok(job)
}
