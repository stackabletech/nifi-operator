//! Builders that assemble Kubernetes resources from a [`ValidatedCluster`].
//!
//! [`ValidatedCluster`]: crate::controller::ValidatedCluster

use std::{marker::PhantomData, str::FromStr};

use snafu::{ResultExt, Snafu};
use stackable_operator::{
    builder::meta::ObjectMetaBuilder,
    kvp::Labels,
    v2::{
        builder::meta::ownerreference_from_resource,
        types::{common::Port, operator::RoleGroupName},
    },
};

use crate::{
    controller::{
        KubernetesResources, Prepared, ValidatedCluster,
        build::resource::{
            config_map::build_rolegroup_config_map,
            listener::{build_group_listener, group_listener_name},
            pdb::build_pdb,
            rbac::{build_role_binding, build_service_account},
            secret::build_secrets,
            service::{build_rolegroup_headless_service, build_rolegroup_metrics_service},
            statefulset::build_node_rolegroup_statefulset,
        },
    },
    crd::NifiRole,
};

pub mod git_sync;
pub mod graceful_shutdown;
pub mod jvm;
pub mod properties;
pub mod proxy_hosts;
pub mod resource;

// Placeholder role-group name for role-level resources (e.g. the per-role `Listener`), which have
// no associated role group. Preserves the historical `app.kubernetes.io/role-group: none` label.
stackable_operator::constant!(pub(crate) PLACEHOLDER_LISTENER_ROLE_GROUP: RoleGroupName = "none");

pub const HTTPS_PORT_NAME: &str = "https";
pub const HTTPS_PORT: Port = Port(8443);
pub const PROTOCOL_PORT_NAME: &str = "protocol";
pub const PROTOCOL_PORT: Port = Port(9088);
pub const BALANCE_PORT_NAME: &str = "balance";
pub const BALANCE_PORT: Port = Port(6243);

/// Loopback address NiFi's management server binds to by default
/// (`org.apache.nifi.management.server.address`, see
/// `ManagementServerProvider.MANAGEMENT_SERVER_DEFAULT_ADDRESS` upstream).
/// The operator pins the JVM system property to this same value (see
/// `build::jvm`), so this constant is the single source of truth for it.
/// Note: a user-supplied `jvmArgumentOverrides` that removes or repoints the
/// corresponding `-D` property would desync this from the actual
/// management-server address, silently breaking both probes.
pub const MANAGEMENT_SERVER_ADDRESS: &str = "127.0.0.1";
/// Port NiFi's management server binds to by default, see
/// [`MANAGEMENT_SERVER_ADDRESS`].
pub const MANAGEMENT_SERVER_PORT: u16 = 52020;

// Filesystem paths shared by multiple builders. Single-consumer paths live in their builder.
pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";
pub const NIFI_PYTHON_WORKING_DIRECTORY: &str = "/nifi-python-working-directory";
/// Mount path of the sensitive-properties key Secret, whose contents are keyed by
/// [`SENSITIVE_PROPERTY_KEY_NAME`](resource::secret::SENSITIVE_PROPERTY_KEY_NAME).
pub const SENSITIVE_PROPERTY_VOLUME_MOUNT: &str = "/stackable/sensitiveproperty";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("failed to build ConfigMap for role group {role_group}"))]
    ConfigMap {
        source: resource::config_map::Error,
        role_group: RoleGroupName,
    },

    #[snafu(display("failed to build StatefulSet for role group {role_group}"))]
    StatefulSet {
        source: resource::statefulset::Error,
        role_group: RoleGroupName,
    },
}

/// Builds every Kubernetes resource for the given validated cluster.
///
/// Does not need a Kubernetes client: every reference to another Kubernetes resource is already
/// dereferenced and validated by this point, so the errors returned here are resource-assembly
/// failures only.
pub fn build(cluster: &ValidatedCluster) -> Result<KubernetesResources<Prepared>, Error> {
    let mut stateful_sets = vec![];
    let mut services = vec![];
    let mut listeners = vec![];
    let mut config_maps = vec![];
    let mut pod_disruption_budgets = vec![];

    // NiFi has a single role (`node`), which must always be present.
    let nifi_role = NifiRole::Node;
    let node_role_group_configs = cluster
        .role_group_configs
        .get(&nifi_role)
        .expect("the nodes role is required by the CRD and validate always inserts it");

    // Role-level resources (one per role): the PodDisruptionBudget and the group Listener.
    let role_config = &cluster.role_config;
    if let Some(pdb) = build_pdb(&role_config.pdb, cluster, &nifi_role) {
        pod_disruption_budgets.push(pdb);
    }
    listeners.push(build_group_listener(
        cluster,
        role_config.listener_class.clone(),
        group_listener_name(cluster, &nifi_role.to_string()),
    ));

    for (role_group_name, rg) in node_role_group_configs {
        services.push(build_rolegroup_headless_service(cluster, role_group_name));
        services.push(build_rolegroup_metrics_service(cluster, role_group_name));

        config_maps.push(
            build_rolegroup_config_map(cluster, role_group_name, rg).context(ConfigMapSnafu {
                role_group: role_group_name.clone(),
            })?,
        );

        let effective_replicas = rg.replicas.map(i32::from);
        stateful_sets.push(
            build_node_rolegroup_statefulset(cluster, role_group_name, rg, effective_replicas)
                .context(StatefulSetSnafu {
                    role_group: role_group_name.clone(),
                })?,
        );
    }

    Ok(KubernetesResources {
        stateful_sets,
        services,
        listeners,
        config_maps,
        secrets: build_secrets(cluster),
        pod_disruption_budgets,
        service_accounts: vec![build_service_account(cluster)],
        role_bindings: vec![build_role_binding(cluster)],
        status: PhantomData,
    })
}

/// Returns an [`ObjectMetaBuilder`] pre-filled with the cluster's namespace, an owner reference
/// back to the cluster, the resource `name` and the given `recommended_labels`.
///
/// Consolidates the metadata chain repeated by the child-resource builders. Call sites that
/// need extra labels/annotations chain them onto the returned builder. The labels are passed in
/// rather than derived here, so callers can pick the variant they need: role-level resources
/// (e.g. the per-role [`Listener`](stackable_operator::crd::listener::v1alpha1::Listener)) use the
/// placeholder role group `none`, and resources that must not change after deployment use the
/// unversioned labels.
pub(crate) fn object_meta(
    cluster: &ValidatedCluster,
    name: impl Into<String>,
    recommended_labels: Labels,
) -> ObjectMetaBuilder {
    let mut builder = ObjectMetaBuilder::new();
    builder
        .name_and_namespace(cluster)
        .name(name)
        .ownerreference(ownerreference_from_resource(cluster, None, Some(true)))
        .with_labels(recommended_labels);
    builder
}

#[cfg(test)]
mod tests {
    use stackable_operator::kube::Resource;

    use super::{build, properties::test_support::minimal_validated_cluster};

    fn sorted_names(resources: &[impl Resource]) -> Vec<&str> {
        let mut names: Vec<&str> = resources
            .iter()
            .filter_map(|resource| resource.meta().name.as_deref())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn build_produces_expected_resources() {
        let cluster = minimal_validated_cluster();
        let resources = build(&cluster).expect("build succeeds");

        // The minimal fixture has a single `default` role group for the `node` role.
        assert_eq!(
            sorted_names(&resources.stateful_sets),
            ["simple-nifi-node-default"]
        );
        assert_eq!(
            sorted_names(&resources.config_maps),
            ["simple-nifi-node-default"]
        );
        // One headless and one metrics Service per role group.
        assert_eq!(resources.services.len(), 2);
        // One group Listener and one PDB for the single `node` role.
        assert_eq!(sorted_names(&resources.listeners), ["simple-nifi-node"]);
        assert_eq!(
            sorted_names(&resources.pod_disruption_budgets),
            ["simple-nifi-node"]
        );
        // The sensitive-properties key Secret, generated because the fixture has none yet. The
        // OIDC admin password Secret is absent because the fixture uses SingleUser authentication.
        assert_eq!(
            sorted_names(&resources.secrets),
            ["simple-nifi-sensitive-property-key"]
        );
        // The cluster-shared RBAC pair.
        assert_eq!(
            sorted_names(&resources.service_accounts),
            ["simple-nifi-serviceaccount"]
        );
        assert_eq!(
            sorted_names(&resources.role_bindings),
            ["simple-nifi-rolebinding"]
        );
    }
}
