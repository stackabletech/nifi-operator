//! Build per-rolegroup `ConfigMap` for the NiFi cluster.

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    kvp::ObjectLabels,
    product_logging::framework::VECTOR_CONFIG_FILE,
    role_utils::RoleGroupRef,
    utils::cluster_info::KubernetesClusterInfo,
    v2::builder::meta::ownerreference_from_resource,
};

use crate::{
    controller::{
        ValidatedCluster,
        build::{
            git_sync,
            properties::{
                ConfigFileName, authorizers, bootstrap_conf, logging, login_identity_providers,
                nifi_properties, security_properties, state_management_xml,
            },
            proxy_hosts,
        },
    },
    crd::{NifiRole, v1alpha1},
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to build metadata"))]
    MetadataBuild {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build bootstrap.conf"))]
    BootstrapConfig {
        #[snafu(source(from(crate::controller::build::Error, Box::new)))]
        source: Box<crate::controller::build::Error>,
    },

    #[snafu(display("failed to prepare NiFi configuration for rolegroup {rolegroup}"))]
    BuildNifiProperties {
        #[snafu(source(from(crate::controller::build::Error, Box::new)))]
        source: Box<crate::controller::build::Error>,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to build ConfigMap for {rolegroup}"))]
    BuildRoleGroupConfig {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to serialize JVM security properties for {}", rolegroup))]
    JvmSecurityProperties {
        source: stackable_operator::v2::config_file_writer::PropertiesWriterError,
        rolegroup: String,
    },

    #[snafu(display("failed to build login-identity-providers configuration"))]
    InvalidNifiAuthenticationConfig {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("the cluster has no rolegroup [{role_group}] in role [{role}]"))]
    MissingRoleGroup { role: String, role_group: String },

    #[snafu(display("failed to build git-sync resources"))]
    BuildGitSyncResources { source: git_sync::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Build the rolegroup [`ConfigMap`] configuring the rolegroup based on the
/// resolved cluster configuration.
///
/// All NiFi configuration is sourced from `cluster`. `recommended_labels` must be built by the
/// caller (typically via `build_recommended_labels`).
pub fn build_rolegroup_config_map(
    cluster: &ValidatedCluster,
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
    recommended_labels: &ObjectLabels<'_, ValidatedCluster>,
    cluster_info: &KubernetesClusterInfo,
) -> Result<ConfigMap> {
    tracing::debug!("building rolegroup ConfigMap");

    let rg = cluster
        .role_group_configs
        .get(&NifiRole::Node)
        .and_then(|groups| groups.get(&rolegroup.role_group))
        .with_context(|| MissingRoleGroupSnafu {
            role: NifiRole::Node.to_string(),
            role_group: rolegroup.role_group.clone(),
        })?;

    let proxy_hosts = proxy_hosts::compute_proxy_hosts(cluster, cluster_info);
    let git_sync_resources =
        git_sync::build_git_sync_resources(cluster, rg).context(BuildGitSyncResourcesSnafu)?;

    let mut cm_builder = ConfigMapBuilder::new();

    cm_builder
        .metadata(
            ObjectMetaBuilder::new()
                .namespace(&cluster.namespace)
                .name(rolegroup.object_name())
                .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
                .with_recommended_labels(recommended_labels)
                .context(MetadataBuildSnafu)?
                .build(),
        )
        .add_data(
            ConfigFileName::BootstrapConf.to_string(),
            bootstrap_conf::build(rg, Some(&cluster.cluster_config.authorization))
                .context(BootstrapConfigSnafu)?,
        )
        .add_data(
            ConfigFileName::NifiProperties.to_string(),
            nifi_properties::build(cluster, rg, &proxy_hosts, &git_sync_resources).with_context(
                |_| BuildNifiPropertiesSnafu {
                    rolegroup: rolegroup.clone(),
                },
            )?,
        )
        .add_data(
            ConfigFileName::StateManagementXml.to_string(),
            state_management_xml::build(&cluster.cluster_config.clustering_backend),
        )
        .add_data(
            ConfigFileName::LoginIdentityProviders.to_string(),
            login_identity_providers::build(cluster)
                .context(InvalidNifiAuthenticationConfigSnafu)?,
        )
        .add_data(
            ConfigFileName::Authorizers.to_string(),
            authorizers::build(cluster),
        )
        .add_data(
            ConfigFileName::SecurityProperties.to_string(),
            security_properties::build(rg).with_context(|_| JvmSecurityPropertiesSnafu {
                rolegroup: rolegroup.role_group.clone(),
            })?,
        );

    if let Some(logback_config) = logging::build_logback_config(&rg.config.logging) {
        cm_builder.add_data(ConfigFileName::Logback.to_string(), logback_config);
    }

    if let Some(vector_config) = logging::build_vector_config(rolegroup, &rg.config.logging) {
        cm_builder.add_data(VECTOR_CONFIG_FILE, vector_config);
    }

    cm_builder
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup.clone(),
        })
}
