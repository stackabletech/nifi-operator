//! Build per-rolegroup `ConfigMap` for the NiFi cluster.

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    product_logging::framework::VECTOR_CONFIG_FILE,
    utils::cluster_info::KubernetesClusterInfo,
    v2::{builder::meta::ownerreference_from_resource, types::operator::RoleGroupName},
};

use crate::{
    controller::{
        ValidatedCluster,
        build::{
            properties::{
                ConfigFileName, authorizers, bootstrap_conf, login_identity_providers,
                nifi_properties, product_logging, security_properties, state_management_xml,
            },
            proxy_hosts,
        },
    },
    crd::NifiRole,
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("failed to build bootstrap.conf"))]
    BootstrapConfig {
        #[snafu(source(from(crate::controller::build::Error, Box::new)))]
        source: Box<crate::controller::build::Error>,
    },

    #[snafu(display("failed to prepare NiFi configuration for rolegroup {rolegroup}"))]
    BuildNifiProperties {
        #[snafu(source(from(crate::controller::build::Error, Box::new)))]
        source: Box<crate::controller::build::Error>,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to build ConfigMap for {rolegroup}"))]
    BuildRoleGroupConfig {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to serialize JVM security properties for {}", rolegroup))]
    JvmSecurityProperties {
        source: stackable_operator::v2::config_file_writer::PropertiesWriterError,
        rolegroup: RoleGroupName,
    },

    #[snafu(display("failed to build login-identity-providers configuration"))]
    InvalidNifiAuthenticationConfig {
        source: crate::security::authentication::Error,
    },

    #[snafu(display("the cluster has no rolegroup [{role_group}] in role [{role}]"))]
    MissingRoleGroup { role: String, role_group: String },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Build the rolegroup [`ConfigMap`] configuring the rolegroup based on the
/// resolved cluster configuration.
///
/// All NiFi configuration is sourced from `cluster`.
pub fn build_rolegroup_config_map(
    cluster: &ValidatedCluster,
    role_group_name: &RoleGroupName,
    cluster_info: &KubernetesClusterInfo,
) -> Result<ConfigMap> {
    tracing::debug!("building rolegroup ConfigMap");

    let rg = cluster
        .role_group_configs
        .get(&NifiRole::Node)
        .and_then(|groups| groups.get(role_group_name))
        .with_context(|| MissingRoleGroupSnafu {
            role: NifiRole::Node.to_string(),
            role_group: role_group_name.to_string(),
        })?;

    let proxy_hosts = proxy_hosts::compute_proxy_hosts(cluster, cluster_info);

    let mut cm_builder = ConfigMapBuilder::new();

    cm_builder
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(cluster)
                .name(
                    cluster
                        .resource_names(role_group_name)
                        .role_group_config_map()
                        .to_string(),
                )
                .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
                .with_labels(cluster.recommended_labels(role_group_name))
                .build(),
        )
        .add_data(
            ConfigFileName::BootstrapConf.to_string(),
            bootstrap_conf::build(rg, Some(&cluster.cluster_config.authorization))
                .context(BootstrapConfigSnafu)?,
        )
        .add_data(
            ConfigFileName::NifiProperties.to_string(),
            nifi_properties::build(cluster, rg, &proxy_hosts).with_context(|_| {
                BuildNifiPropertiesSnafu {
                    rolegroup: role_group_name.clone(),
                }
            })?,
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
                rolegroup: role_group_name.clone(),
            })?,
        );

    if let Some(logback_config) = product_logging::build_logback_config(&rg.config.logging) {
        cm_builder.add_data(ConfigFileName::Logback.to_string(), logback_config);
    }

    if rg.config.logging.enable_vector_agent {
        cm_builder.add_data(
            VECTOR_CONFIG_FILE,
            product_logging::vector_config_file_content(),
        );
    }

    cm_builder
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: role_group_name.clone(),
        })
}
