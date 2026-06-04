//! Builds the logback and Vector logging configuration for the rolegroup `ConfigMap`.

use stackable_operator::{
    memory::BinaryMultiple,
    product_logging::{
        self,
        spec::{ContainerLogConfig, ContainerLogConfigChoice, Logging},
    },
    role_utils::RoleGroupRef,
};

use crate::crd::{Container, MAX_NIFI_LOG_FILES_SIZE, STACKABLE_LOG_DIR, v1alpha1};

pub const NIFI_LOG_FILE: &str = "nifi.log4j.xml";

const CONSOLE_CONVERSION_PATTERN: &str = "%date %level [%thread] %logger{40} %msg%n";
// This is required to remove double entries in the nifi.log4j.xml as well as nested
// console output like: "<timestamp> <loglevel> ... <timestamp> <loglevel> ...
const ADDITIONAL_LOGBACK_CONFIG: &str = r#"  <appender name="PASSTHROUGH" class="ch.qos.logback.core.ConsoleAppender">
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

/// Renders the `logback.xml` for the NiFi container.
///
/// Returns `None` when the container uses a custom log ConfigMap instead of the operator's
/// automatic logging configuration.
pub fn build_logback_config(logging: &Logging<Container>) -> Option<String> {
    let ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    } = logging.containers.get(&Container::Nifi)?
    else {
        return None;
    };

    Some(product_logging::framework::create_logback_config(
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
        Some(ADDITIONAL_LOGBACK_CONFIG),
    ))
}

/// Renders the Vector agent config (`vector.yaml`).
///
/// Returns `None` when the Vector agent is disabled for this role group.
pub fn build_vector_config(
    rolegroup: &RoleGroupRef<v1alpha1::NifiCluster>,
    logging: &Logging<Container>,
) -> Option<String> {
    if !logging.enable_vector_agent {
        return None;
    }

    let vector_log_config = if let Some(ContainerLogConfig {
        choice: Some(ContainerLogConfigChoice::Automatic(log_config)),
    }) = logging.containers.get(&Container::Vector)
    {
        Some(log_config)
    } else {
        None
    };

    Some(product_logging::framework::create_vector_config(
        rolegroup,
        vector_log_config,
    ))
}
