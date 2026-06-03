//! Build per-rolegroup `ConfigMap` for the NiFi cluster.

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    crd::git_sync,
    k8s_openapi::api::core::v1::ConfigMap,
    kvp::ObjectLabels,
    role_utils::RoleGroupRef,
};

use crate::{
    controller::{
        build::properties::{
            ConfigFileName, authorizers, bootstrap_conf, login_identity_providers, nifi_properties,
            security_properties, state_management_xml,
        },
        validate::{NifiRoleGroupConfig, ValidatedCluster},
    },
    crd::{NifiRoleType, v1alpha1},
    product_logging::extend_role_group_config_map,
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("object is missing metadata to build owner reference"))]
    ObjectMissingMetadataForOwnerRef {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build metadata"))]
    MetadataBuild {
        source: stackable_operator::builder::meta::Error,
    },

    #[snafu(display("failed to build bootstrap.conf"))]
    BootstrapConfig {
        #[snafu(source(from(crate::config::Error, Box::new)))]
        source: Box<crate::config::Error>,
    },

    #[snafu(display("failed to prepare NiFi configuration for rolegroup {rolegroup}"))]
    BuildProductConfig {
        #[snafu(source(from(crate::config::Error, Box::new)))]
        source: Box<crate::config::Error>,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to add the logging configuration to the ConfigMap [{cm_name}]"))]
    InvalidLoggingConfig {
        source: crate::product_logging::Error,
        cm_name: String,
    },

    #[snafu(display("failed to build ConfigMap for {rolegroup}"))]
    BuildRoleGroupConfig {
        source: stackable_operator::builder::configmap::Error,
        rolegroup: RoleGroupRef<v1alpha1::NifiCluster>,
    },

    #[snafu(display("failed to serialize JVM security properties for {}", rolegroup))]
    JvmSecurityProperties {
        source: crate::controller::build::properties::writer::PropertiesWriterError,
        rolegroup: String,
    },

    #[snafu(display("failed to build login-identity-providers configuration"))]
    InvalidNifiAuthenticationConfig {
        source: crate::security::authentication::Error,
    },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Build the rolegroup [`ConfigMap`] configuring the rolegroup based on the
/// resolved cluster configuration.
///
/// The only use of `owner` is for the OwnerReference and `name_and_namespace`.
/// All other NiFi configuration is sourced from `cluster` or `rg`.
/// `recommended_labels` must be built by the caller (typically via `build_recommended_labels`).
pub fn build_rolegroup_config_map(
    owner: &v1alpha1::NifiCluster,
    cluster: &ValidatedCluster,
    rg: &NifiRoleGroupConfig,
    role: &NifiRoleType,
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
    git_sync_resources: &git_sync::v1alpha2::GitSyncResources,
    recommended_labels: &ObjectLabels<'_, v1alpha1::NifiCluster>,
) -> Result<ConfigMap> {
    tracing::debug!("building rolegroup ConfigMap");

    let mut cm_builder = ConfigMapBuilder::new();

    cm_builder
        .metadata(
            ObjectMetaBuilder::new()
                .name_and_namespace(owner)
                .name(rolegroup.object_name())
                .ownerreference_from_resource(owner, None, Some(true))
                .context(ObjectMissingMetadataForOwnerRefSnafu)?
                .with_recommended_labels(recommended_labels)
                .context(MetadataBuildSnafu)?
                .build(),
        )
        .add_data(
            ConfigFileName::BootstrapConf.to_string(),
            bootstrap_conf::build(
                rg,
                role,
                &rolegroup.role_group,
                Some(&cluster.cluster_config.authorization),
            )
            .context(BootstrapConfigSnafu)?,
        )
        .add_data(
            ConfigFileName::NifiProperties.to_string(),
            nifi_properties::build(cluster, rg, git_sync_resources).with_context(|_| {
                BuildProductConfigSnafu {
                    rolegroup: rolegroup.clone(),
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
            // TODO: authorizers::build currently takes a raw &NifiCluster; once migrated
            // to ValidatedCluster this `owner` arg can be removed.
            authorizers::build(cluster, owner),
        )
        .add_data(
            ConfigFileName::SecurityProperties.to_string(),
            security_properties::build(rg).with_context(|_| JvmSecurityPropertiesSnafu {
                rolegroup: rolegroup.role_group.clone(),
            })?,
        );

    extend_role_group_config_map(rolegroup, &rg.config.logging, &mut cm_builder).context(
        InvalidLoggingConfigSnafu {
            cm_name: rolegroup.object_name(),
        },
    )?;

    cm_builder
        .build()
        .with_context(|_| BuildRoleGroupConfigSnafu {
            rolegroup: rolegroup.clone(),
        })
}
