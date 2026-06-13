use stackable_operator::{
    commons::pdb::PdbConfig, k8s_openapi::api::policy::v1::PodDisruptionBudget,
    v2::builder::pdb::pod_disruption_budget_builder_with_role,
};

use crate::{
    controller::{ValidatedCluster, controller_name, operator_name, product_name},
    crd::NifiRole,
};

/// Builds the [`PodDisruptionBudget`] for the given `role`, or `None` if PDBs are disabled.
pub fn build_pdb(
    pdb: &PdbConfig,
    cluster: &ValidatedCluster,
    role: &NifiRole,
) -> Option<PodDisruptionBudget> {
    if !pdb.enabled {
        return None;
    }
    let max_unavailable = pdb.max_unavailable.unwrap_or(match role {
        NifiRole::Node => max_unavailable_nodes(),
    });
    let pdb = pod_disruption_budget_builder_with_role(
        cluster,
        &product_name(),
        &ValidatedCluster::role_name(),
        &operator_name(),
        &controller_name(),
    )
    .with_max_unavailable(max_unavailable)
    .build();

    Some(pdb)
}

fn max_unavailable_nodes() -> u16 {
    1
}
