//! Builds the exec-based startup and readiness probes that check NiFi 2.x's
//! local, unauthenticated management-server endpoints (`/health` and
//! `/health/cluster`).
//!
//! The management server binds `127.0.0.1` only, so these must be `exec`
//! probes using `curl` from inside the container - a `httpGet` probe cannot
//! reach a loopback-only address.

use stackable_operator::k8s_openapi::{
    api::core::v1::{ExecAction, Probe, TCPSocketAction},
    apimachinery::pkg::util::intstr::IntOrString,
};

use crate::controller::build::{HTTPS_PORT_NAME, MANAGEMENT_SERVER_PORT};

fn management_health_exec(path: &str) -> ExecAction {
    ExecAction {
        command: Some(vec![
            "/bin/bash".to_string(),
            "-euo".to_string(),
            "pipefail".to_string(),
            "-c".to_string(),
            format!(
                "curl --fail --silent --show-error --output /dev/null http://127.0.0.1:{MANAGEMENT_SERVER_PORT}{path}"
            ),
        ]),
    }
}
pub fn management_startup_probe() -> Probe {
    Probe {
        initial_delay_seconds: Some(10),
        period_seconds: Some(10),
        timeout_seconds: Some(5),
        failure_threshold: Some(20 * 6),
        exec: Some(management_health_exec("/health")),
        ..Probe::default()
    }
}
pub fn management_readiness_probe() -> Probe {
    Probe {
        period_seconds: Some(10),
        timeout_seconds: Some(5),
        failure_threshold: Some(3),
        exec: Some(management_health_exec("/health/cluster")),
        ..Probe::default()
    }
}

pub fn tcp_liveliness_probe() -> Probe {
    Probe {
        initial_delay_seconds: Some(10),
        period_seconds: Some(10),
        tcp_socket: Some(TCPSocketAction {
            port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
            ..TCPSocketAction::default()
        }),
        ..Probe::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_probe_execs_curl_against_health_endpoint() {
        let probe = management_startup_probe();

        let command = probe
            .exec
            .expect("startup probe must be an exec probe")
            .command
            .expect("exec action must have a command");
        let script = command.last().expect("bash -c script argument");

        assert!(
            script.contains("http://127.0.0.1:52020/health") && !script.contains("/health/cluster"),
            "expected curl against /health, got: {script}"
        );
        assert_eq!(probe.failure_threshold, Some(120));
        assert_eq!(probe.timeout_seconds, Some(5));
        assert!(
            probe.tcp_socket.is_none(),
            "must not fall back to tcp_socket"
        );
    }

    #[test]
    fn readiness_probe_execs_curl_against_cluster_health_endpoint() {
        let probe = management_readiness_probe();

        let command = probe
            .exec
            .expect("readiness probe must be an exec probe")
            .command
            .expect("exec action must have a command");
        let script = command.last().expect("bash -c script argument");

        assert!(
            script.contains("http://127.0.0.1:52020/health/cluster"),
            "expected curl against /health/cluster, got: {script}"
        );
        assert_eq!(probe.failure_threshold, Some(3));
        assert_eq!(probe.timeout_seconds, Some(5));
        assert_eq!(
            probe.initial_delay_seconds, None,
            "readiness probe delay is redundant: k8s already suppresses readiness checks \
             until the startup probe succeeds"
        );
    }

    #[test]
    fn probes_use_bash_pipefail_wrapper_not_bare_curl_argv() {
        for probe in [management_startup_probe(), management_readiness_probe()] {
            let command = probe.exec.unwrap().command.unwrap();
            assert_eq!(
                command[..4],
                [
                    "/bin/bash".to_string(),
                    "-euo".to_string(),
                    "pipefail".to_string(),
                    "-c".to_string(),
                ],
                "exec command must follow the repo's bash -euo pipefail -c convention"
            );
        }
    }
}
