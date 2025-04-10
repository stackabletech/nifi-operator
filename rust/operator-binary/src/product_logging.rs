use snafu::Snafu;
use stackable_operator::{
    builder::configmap::ConfigMapBuilder,
    memory::BinaryMultiple,
    product_logging::{
        self,
        spec::{ContainerLogConfig, ContainerLogConfigChoice, Logging},
    },
    role_utils::RoleGroupRef,
};

use crate::crd::{Container, MAX_NIFI_LOG_FILES_SIZE, STACKABLE_LOG_DIR, v1alpha1};

#[derive(Snafu, Debug)]
pub enum Error {
    #[snafu(display("object has no namespace"))]
    ObjectHasNoNamespace,
    #[snafu(display("failed to retrieve the ConfigMap {cm_name}"))]
    ConfigMapNotFound {
        source: stackable_operator::client::Error,
        cm_name: String,
    },
    #[snafu(display("failed to retrieve the entry {entry} for ConfigMap {cm_name}"))]
    MissingConfigMapEntry {
        entry: &'static str,
        cm_name: String,
    },
    #[snafu(display("crd validation failure"))]
    CrdValidationFailure { source: crate::crd::Error },
    #[snafu(display("vectorAggregatorConfigMapName must be set"))]
    MissingVectorAggregatorAddress,
}

type Result<T, E = Error> = std::result::Result<T, E>;

pub const LOGBACK_CONFIG_FILE: &str = "logback.xml";
pub const NIFI_LOG_FILE: &str = "nifi.log4j.xml";

const CONSOLE_CONVERSION_PATTERN: &str = "%date %level [%thread] %logger{40} %msg%n";
// This is required to remove double entries in the nifi.log4j.xml as well as nested
// console output like: "<timestamp> <loglevel> ... <timestamp> <loglevel> ...
const ADDITONAL_LOGBACK_CONFIG: &str = r#"  <appender name="PASSTHROUGH" class="ch.qos.logback.core.ConsoleAppender">
    <encoder>
      <pattern>%msg%n</pattern>
    </encoder>
  </appender>

  <logger name="org.apache.nifi.StdOut" level="INFO" additivity="false">
    <appender-ref ref="PASSTHROUGH" />
  </logger>

  <logger name="org.apache.nifi.StdErr" level="INFO" additivity="false">
    <appender-ref ref="PASSTHROUGH" />
  </logger>
"#;

/// Extend the role group ConfigMap with logging and Vector configurations
pub fn extend_role_group_config_map(
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
    logging: &Logging<Container>,
    cm_builder: &mut ConfigMapBuilder,
) -> Result<()> {
    if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = logging.containers.get(&Container::Nifi)
    {
        cm_builder.add_data(
            LOGBACK_CONFIG_FILE,
            product_logging::framework::create_logback_config(
                &format!(
                    "{STACKABLE_LOG_DIR}/{container}",
                    container = Container::Nifi
                ),
                NIFI_LOG_FILE,
                MAX_NIFI_LOG_FILES_SIZE
                    .scale_to(BinaryMultiple::Mebi)
                    .floor()
                    .value as u32,
                CONSOLE_CONVERSION_PATTERN,
                log_config,
                Some(ADDITONAL_LOGBACK_CONFIG),
            ),
        );
    }

    let vector_log_config = if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = logging.containers.get(&Container::Vector)
    {
        Some(log_config)
    } else {
        None
    };

    if logging.enable_vector_agent {
        cm_builder.add_data(
            product_logging::framework::VECTOR_CONFIG_FILE,
            product_logging::framework::create_vector_config(rolegroup, vector_log_config),
        );
    }

    Ok(())
}
