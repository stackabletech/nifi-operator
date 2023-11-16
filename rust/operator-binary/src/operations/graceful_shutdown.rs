use std::collections::BTreeMap;

use snafu::{ResultExt, Snafu};
use stackable_nifi_crd::NifiConfig;
use stackable_operator::builder::PodBuilder;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to set terminationGracePeriod"))]
    SetTerminationGracePeriod {
        source: stackable_operator::builder::pod::Error,
    },
}

pub fn graceful_shutdown_config_properties(config: &NifiConfig) -> BTreeMap<String, String> {
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
    merged_config: &NifiConfig,
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
