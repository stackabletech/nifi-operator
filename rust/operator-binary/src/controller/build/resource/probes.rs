//! Builds the exec-based startup and readiness probes that check NiFi 2.x's
//! local, unauthenticated management-server endpoints (`/health` and
//! `/health/cluster`).
//!
//! The management server binds `127.0.0.1` only, so these must be `exec`
//! probes using `curl` from inside the container - a `httpGet` probe cannot
//! reach a loopback-only address.

use stackable_operator::{
    builder::pod::probe::{ProbeAction, ProbeBuilder},
    k8s_openapi::{
        api::core::v1::{Probe, TCPSocketAction},
        apimachinery::pkg::util::intstr::IntOrString,
    },
    shared::time::Duration,
};

use crate::controller::build::{
    HTTPS_PORT_NAME, MANAGEMENT_SERVER_ADDRESS, MANAGEMENT_SERVER_PORT,
};

fn common_probe_baseline(path: &str) -> ProbeBuilder<ProbeAction, Duration> {
    ProbeBuilder::exec_command([
        "/bin/bash".to_string(),
        "-euo".to_string(),
        "pipefail".to_string(),
        "-c".to_string(),
        format!("curl --fail --silent --show-error --output /dev/null http://{MANAGEMENT_SERVER_ADDRESS}:{MANAGEMENT_SERVER_PORT}{path}"),
    ])
    .with_period(Duration::from_secs(10))
    .with_timeout(Duration::from_secs(5))
}

pub fn management_startup_probe() -> Probe {
    common_probe_baseline("/health")
        .with_initial_delay(Duration::from_secs(10))
        .with_failure_threshold_duration(Duration::from_minutes_unchecked(20))
        .expect("static period is non-zero")
        .build()
        .expect("static duration is not too long")
}

pub fn management_readiness_probe() -> Probe {
    common_probe_baseline("/health/cluster")
        .with_failure_threshold_duration(Duration::from_secs(30))
        .expect("static period is non-zero")
        .build()
        .expect("static duration is not too long")
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

        let cluster_health_url =
            format!("http://{MANAGEMENT_SERVER_ADDRESS}:{MANAGEMENT_SERVER_PORT}/health/cluster");
        assert!(
            script.contains(&cluster_health_url),
            "expected curl against /health/cluster, got: {script}"
        );
        assert_eq!(probe.failure_threshold, Some(3));
        assert_eq!(probe.timeout_seconds, Some(5));
        assert_eq!(
            probe.initial_delay_seconds,
            Some(0),
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
