//! Builder for `login-identity-providers.xml`.

use crate::controller::validate::ValidatedCluster;

pub fn build(cluster: &ValidatedCluster) -> Result<String, crate::security::authentication::Error> {
    cluster
        .cluster_config
        .authentication
        .get_authentication_config()
}
