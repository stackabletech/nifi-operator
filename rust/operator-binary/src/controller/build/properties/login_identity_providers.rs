//! Builder for `login-identity-providers.xml`.

use crate::controller::validate::ValidatedCluster;

pub fn build(cluster: &ValidatedCluster) -> Result<String, crate::security::authentication::Error> {
    cluster
        .cluster_config
        .authentication
        .get_authentication_config()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

    #[test]
    fn test_build_returns_ok_with_expected_structure() {
        let cluster = minimal_validated_cluster();
        let xml = build(&cluster).expect("build should succeed for SingleUser auth");

        assert!(
            xml.contains("<loginIdentityProviders>"),
            "output must contain <loginIdentityProviders> root element"
        );
        assert!(
            xml.contains("SingleUserLoginIdentityProvider"),
            "expected SingleUserLoginIdentityProvider class for SingleUser auth"
        );
        assert!(
            xml.contains("</loginIdentityProviders>"),
            "output must contain closing </loginIdentityProviders> tag"
        );
    }
}
