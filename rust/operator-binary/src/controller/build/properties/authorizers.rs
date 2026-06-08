//! Builder for `authorizers.xml`.

use crate::controller::validate::ValidatedCluster;

pub fn build(cluster: &ValidatedCluster) -> String {
    cluster
        .cluster_config
        .authorization
        .get_authorizers_config(cluster.name.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::build::properties::test_support::minimal_validated_cluster;

    #[test]
    fn test_build_returns_non_empty_xml_with_authorizers_root() {
        let cluster = minimal_validated_cluster();

        let xml = build(&cluster);

        assert!(!xml.is_empty(), "authorizers.xml should not be empty");
        assert!(
            xml.contains("<authorizers>"),
            "output must contain <authorizers> root element"
        );
        assert!(
            xml.contains("</authorizers>"),
            "output must contain closing </authorizers> tag"
        );
        // For SingleUser authorization we expect the SingleUserAuthorizer class
        assert!(
            xml.contains("SingleUserAuthorizer"),
            "expected SingleUserAuthorizer for SingleUser authorization"
        );
    }
}
