use std::collections::BTreeMap;

use snafu::{ResultExt, Snafu};
use stackable_operator::builder::pod::PodBuilder;

use crate::controller::ValidatedNifiConfig;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to set terminationGracePeriod"))]
    SetTerminationGracePeriod {
        source: stackable_operator::builder::pod::Error,
    },
}

pub fn graceful_shutdown_config_properties(
    config: &ValidatedNifiConfig,
) -> BTreeMap<String, String> {
    let mut graceful_shutdown_properties = BTreeMap::new();
    if let Some(graceful_shutdown_timeout) = config.graceful_shutdown_timeout {
        graceful_shutdown_properties.insert(
            "graceful.shutdown.seconds".to_string(),
            graceful_shutdown_timeout.as_secs().to_string(),
        );
    }
    graceful_shutdown_properties
}

pub fn add_graceful_shutdown_config(
    merged_config: &ValidatedNifiConfig,
    pod_builder: &mut PodBuilder,
) -> Result<(), Error> {
    // This must be always set by the merge mechanism, as we provide a default value,
    // users can not disable graceful shutdown.
    if let Some(graceful_shutdown_timeout) = merged_config.graceful_shutdown_timeout {
        pod_builder
            .termination_grace_period(&graceful_shutdown_timeout)
            .context(SetTerminationGracePeriodSnafu)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::graceful_shutdown_config_properties;
    use crate::controller::build::properties::test_support::{
        default_rg, minimal_validated_cluster,
    };

    /// The merge mechanism always provides a graceful-shutdown timeout, so the property is set.
    #[test]
    fn default_config_sets_graceful_shutdown_seconds() {
        let cluster = minimal_validated_cluster();
        let properties = graceful_shutdown_config_properties(&default_rg(&cluster).config);
        assert!(properties.contains_key("graceful.shutdown.seconds"));
    }
}
