//! Builder for `authorizers.xml`.

use crate::{controller::validate::ValidatedCluster, crd::v1alpha1};

pub fn build(cluster: &ValidatedCluster, nifi: &v1alpha1::NifiCluster) -> String {
    // TODO(follow-up PR): narrow get_authorizers_config to resolved fields on ValidatedCluster instead of taking the full NifiCluster.
    cluster
        .cluster_config
        .authorization
        .get_authorizers_config(nifi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::build::properties::test_support::{
        MINIMAL_NIFI_YAML, minimal_validated_cluster,
    };

    #[test]
    fn test_build_returns_non_empty_xml_with_authorizers_root() {
        let cluster = minimal_validated_cluster();
        let nifi: v1alpha1::NifiCluster =
            serde_yaml::from_str(MINIMAL_NIFI_YAML).expect("invalid test YAML");

        let xml = build(&cluster, &nifi);

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
