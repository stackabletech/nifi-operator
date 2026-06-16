//! Builds the logback and Vector logging configuration for the rolegroup `ConfigMap`.

use stackable_operator::{
    memory::BinaryMultiple,
    product_logging::{
        self,
        spec::{ContainerLogConfig, ContainerLogConfigChoice, Logging},
    },
    v2::product_logging::framework::STACKABLE_LOG_DIR,
};

use crate::crd::{Container, MAX_NIFI_LOG_FILES_SIZE};

pub const NIFI_LOG_FILE: &str = "nifi.log4j.xml";

/// The Vector agent configuration (`vector.yaml`).
///
/// Embedded statically and shipped in the rolegroup `ConfigMap`; the per-rolegroup values
/// (namespace, cluster/role/role-group, aggregator address, log levels) are injected as
/// environment variables by the Vector container (see
/// [`stackable_operator::v2::product_logging::framework::vector_container`]) and substituted by
/// Vector at runtime.
const VECTOR_CONFIG: &str = include_str!("vector.yaml");

/// Returns the Vector agent config (`vector.yaml`) content.
pub fn vector_config_file_content() -> String {
    VECTOR_CONFIG.to_owned()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_config_file_content() {
        let content = vector_config_file_content();
        assert!(!content.is_empty());
        // NiFi logs via logback's `XMLLayout` (log4j XML), so the `files_log4j` source
        assert!(content.contains("files_log4j"));
    }
}
