//! Build per-rolegroup `ConfigMap` for the NiFi cluster.

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::{
    builder::{configmap::ConfigMapBuilder, meta::ObjectMetaBuilder},
    k8s_openapi::api::core::v1::ConfigMap,
    kvp::ObjectLabels,
    product_logging::framework::VECTOR_CONFIG_FILE,
    role_utils::RoleGroupRef,
};

use crate::{
    controller::{
        build::properties::{
            ConfigFileName, authorizers, bootstrap_conf, logging, login_identity_providers,
            nifi_properties, security_properties, state_management_xml,
        },
        validate::ValidatedCluster,
    },
    crd::{NifiRole, v1alpha1},
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
    BuildNifiProperties {
        #[snafu(source(from(crate::config::Error, Box::new)))]
        source: Box<crate::config::Error>,
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

    #[snafu(display("object has no nodes defined"))]
    NoNodesDefined,

    #[snafu(display("the cluster has no rolegroup [{role_group}] in role [{role}]"))]
    MissingRoleGroup { role: String, role_group: String },

    #[snafu(display("missing git-sync resources for rolegroup [{role_group}]"))]
    MissingGitSyncResources { role_group: String },
}

type Result<T, E = Error> = std::result::Result<T, E>;

/// Build the rolegroup [`ConfigMap`] configuring the rolegroup based on the
/// resolved cluster configuration.
///
/// All NiFi configuration is sourced from `cluster`. The only use of `owner` is for
/// the OwnerReference, `name_and_namespace`, and the raw role spec used for JVM
/// argument merging. `recommended_labels` must be built by the caller (typically via
/// `build_recommended_labels`).
pub fn build_rolegroup_config_map(
    cluster: &ValidatedCluster,
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
    recommended_labels: &ObjectLabels<'_, v1alpha1::NifiCluster>,
    owner: &v1alpha1::NifiCluster,
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

    // The raw role spec is only needed for JVM argument merging in `bootstrap_conf`.
    let role = owner.spec.nodes.as_ref().context(NoNodesDefinedSnafu)?;

    let git_sync_resources = cluster
        .git_sync_resources
        .get(&rolegroup.role_group)
        .with_context(|| MissingGitSyncResourcesSnafu {
            role_group: rolegroup.role_group.clone(),
        })?;

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
                BuildNifiPropertiesSnafu {
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
