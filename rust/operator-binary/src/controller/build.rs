//! Builders that assemble Kubernetes resources from a [`ValidatedCluster`].
//!
//! [`ValidatedCluster`]: crate::controller::ValidatedCluster

use std::str::FromStr;

use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::v2::types::{common::Port, operator::RoleGroupName};

use crate::{
    controller::{
        KubernetesResources, ValidatedCluster,
        build::resource::{
            config_map::build_rolegroup_config_map,
            listener::{build_group_listener, group_listener_name},
            pdb::build_pdb,
            rbac::{build_role_binding, build_service_account},
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

// Filesystem paths shared by multiple builders. Single-consumer paths live in their builder.
pub const NIFI_CONFIG_DIRECTORY: &str = "/stackable/nifi/conf";
pub const NIFI_PYTHON_WORKING_DIRECTORY: &str = "/nifi-python-working-directory";

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("NifiCluster has no nodes role defined"))]
    NoNodesDefined,

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
pub fn build(cluster: &ValidatedCluster) -> Result<KubernetesResources, Error> {
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
        .context(NoNodesDefinedSnafu)?;

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
        pod_disruption_budgets,
        service_accounts: vec![build_service_account(cluster)],
        role_bindings: vec![build_role_binding(cluster)],
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

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
    }

    /// Locks the RBAC resource names, the roleRef, and the recommended label set against
    /// accidental drift. The fixture's cluster name deliberately differs from the product name so
    /// that swapped `name`/`instance` label values cannot pass unnoticed.
    ///
    /// The version label is the bare `2.9.0` because the fixture hand-builds its
    /// [`ResolvedProductImage`](stackable_operator::commons::product_image_selection::ResolvedProductImage)
    /// instead of resolving it (which would append the `-stackable…` suffix).
    #[test]
    fn build_produces_rbac() {
        let cluster = minimal_validated_cluster();
        let resources = build(&cluster).expect("build succeeds");

        assert_eq!(
            sorted_names(&resources.service_accounts),
            ["simple-nifi-serviceaccount"]
        );
        assert_eq!(
            sorted_names(&resources.role_bindings),
            ["simple-nifi-rolebinding"]
        );

        let expected_labels = BTreeMap::from(
            [
                ("app.kubernetes.io/component", "none"),
                ("app.kubernetes.io/instance", "simple-nifi"),
                (
                    "app.kubernetes.io/managed-by",
                    "nifi.stackable.tech_nificluster",
                ),
                ("app.kubernetes.io/name", "nifi"),
                ("app.kubernetes.io/role-group", "none"),
                ("app.kubernetes.io/version", "2.9.0"),
                ("stackable.tech/vendor", "Stackable"),
            ]
            .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        let service_account = resources
            .service_accounts
            .first()
            .expect("a ServiceAccount is built");
        assert_eq!(
            service_account.metadata.labels,
            Some(expected_labels.clone())
        );

        let role_binding = resources
            .role_bindings
            .first()
            .expect("a RoleBinding is built");
        assert_eq!(role_binding.metadata.labels, Some(expected_labels));
        assert_eq!(role_binding.role_ref.name, "nifi-clusterrole");
    }
}
