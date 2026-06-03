//! Builder for `authorizers.xml`.

use crate::{controller::validate::ValidatedCluster, crd::v1alpha1};

pub fn build(cluster: &ValidatedCluster, nifi: &v1alpha1::NifiCluster) -> String {
    // TODO(follow-up PR): narrow get_authorizers_config to resolved fields on ValidatedCluster instead of taking the full NifiCluster.
    cluster
        .cluster_config
        .authorization
        .get_authorizers_config(nifi)
}
