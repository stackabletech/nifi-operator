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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use stackable_operator::{
        commons::pdb::PdbConfig, k8s_openapi::apimachinery::pkg::util::intstr::IntOrString,
    };

    use super::*;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

    #[test]
    fn build_pdb_returns_none_when_disabled() {
        let cluster = minimal_validated_cluster();
        let pdb = PdbConfig {
            enabled: false,
            max_unavailable: None,
        };
        assert!(build_pdb(&pdb, &cluster, &NifiRole::Node).is_none());
    }

    #[test]
    fn build_pdb_uses_explicit_max_unavailable() {
        let cluster = minimal_validated_cluster();
        let pdb = PdbConfig {
            enabled: true,
            max_unavailable: Some(2),
        };

        let spec = build_pdb(&pdb, &cluster, &NifiRole::Node)
            .expect("an enabled PDB must be built")
            .spec
            .expect("the PDB must have a spec");
        assert_eq!(Some(IntOrString::Int(2)), spec.max_unavailable);
    }

    #[test]
    fn build_pdb_defaults_max_unavailable_to_one() {
        let cluster = minimal_validated_cluster();
        let pdb = PdbConfig {
            enabled: true,
            max_unavailable: None,
        };

        let spec = build_pdb(&pdb, &cluster, &NifiRole::Node)
            .expect("an enabled PDB must be built")
            .spec
            .expect("the PDB must have a spec");
        assert_eq!(Some(IntOrString::Int(1)), spec.max_unavailable);
    }
}
