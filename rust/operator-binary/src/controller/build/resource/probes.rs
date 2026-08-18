//! Builds the exec-based startup and readiness probes that check NiFi 2.x's
//! local, unauthenticated management-server endpoints (`/health` and
//! `/health/cluster`).
//!
//! The management server binds `127.0.0.1` only, so these must be `exec`
//! probes using `curl` from inside the container - a `httpGet` probe cannot
//! reach a loopback-only address.

use stackable_operator::{
    builder::pod::probe::ProbeBuilder,
    k8s_openapi::{
        api::core::v1::{Probe, TCPSocketAction},
        apimachinery::pkg::util::intstr::IntOrString,
    },
    shared::time::Duration,
};

use crate::controller::build::{
    HTTPS_PORT_NAME, MANAGEMENT_SERVER_ADDRESS, MANAGEMENT_SERVER_PORT,
};

fn management_health_exec_command() -> [String; 5] {
    [
        "/bin/bash".to_string(),
        "-euo".to_string(),
        "pipefail".to_string(),
        "-c".to_string(),
        format!(
            "curl --fail --silent --show-error --output /dev/null http://{MANAGEMENT_SERVER_ADDRESS}:{MANAGEMENT_SERVER_PORT}/health"
        ),
    ]
}

/// NiFi's `/health/cluster` endpoint returns HTTP 200 for both `CONNECTING`
/// and `CONNECTED`, so the readiness probe greps for that instead of only
/// checking the return code.
fn management_cluster_connected_exec_command() -> [String; 5] {
    [
        "/bin/bash".to_string(),
        "-euo".to_string(),
        "pipefail".to_string(),
        "-c".to_string(),
        format!(
            "curl --fail --silent --show-error http://{MANAGEMENT_SERVER_ADDRESS}:{MANAGEMENT_SERVER_PORT}/health/cluster | grep -q 'Cluster Status: CONNECTED'"
        ),
    ]
}

/// The startup probe fails after roughly 20 minutes.
///
/// Nifi might take a very long time to start up due to the following factors:
/// - JVM cold starts are usually slow.
/// - It expands NAR bundles (each processor/controller-service bundle) into
///   the working directory and builds a classloader per NAR.
/// - It replays/rolls back the FlowFile repository  to reconstruct in-flight
///   FlowFile state.
///   If the previous shutdown wasn't clean, or there's a large backlog of
///   in-flight FlowFiles, this replay can take a while.
///   Content and provenance repositories also do startup housekeeping.
/// - Large flow definitions take longer to deserialize and instantiate into
///   the running flow controller graph.
pub fn management_startup_probe() -> Probe {
    ProbeBuilder::exec_command(management_health_exec_command())
        .with_period(Duration::from_secs(10))
        .with_initial_delay(Duration::from_secs(10))
        .with_timeout(Duration::from_secs(5))
        .with_failure_threshold_duration(Duration::from_minutes_unchecked(20))
        .expect("static period is non-zero")
        .build()
        .expect("the startup probe's durations must fit into an i32")
}

/// The readiness probe fails after roughly 5 minutes.
///
/// In clustered mode, a node connecting has to talk to the cluster coordinator,
/// participate in flow election/inheritance, and reconcile its local flow against
/// the cluster's.
pub fn management_readiness_probe() -> Probe {
    ProbeBuilder::exec_command(management_cluster_connected_exec_command())
        .with_period(Duration::from_secs(10))
        .with_timeout(Duration::from_secs(5))
        .with_failure_threshold_duration(Duration::from_minutes_unchecked(5))
        .expect("static period is non-zero")
        .build()
        .expect("the readiness probe's durations must fit into an i32")
}

pub fn tcp_liveness_probe() -> Probe {
    ProbeBuilder::tcp_socket(TCPSocketAction {
        port: IntOrString::String(HTTPS_PORT_NAME.to_string()),
        ..Default::default()
    })
    .with_period(Duration::from_secs(10))
    .build()
    .expect("the liveness probe's durations must fit into an i32")
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
        assert_eq!(probe.failure_threshold, Some(30));
        assert_eq!(probe.timeout_seconds, Some(5));
        assert_eq!(
            probe.initial_delay_seconds,
            Some(0),
            "readiness probe delay is redundant: k8s already suppresses readiness checks \
             until the startup probe succeeds"
        );
    }

    #[test]
    fn readiness_probe_greps_body_for_connected_status() {
        // NiFi's /health/cluster endpoint returns HTTP 200 for both
        // CONNECTING and CONNECTED nodes, so the return code alone can't
        // distinguish a node that is still joining from one that has
        // actually joined. The probe must inspect the response body.
        let probe = management_readiness_probe();

        let command = probe
            .exec
            .expect("readiness probe must be an exec probe")
            .command
            .expect("exec action must have a command");
        let script = command.last().expect("bash -c script argument");

        assert!(
            script.contains("grep") && script.contains("Cluster Status: CONNECTED"),
            "expected the probe to grep the response body for \"Cluster Status: CONNECTED\", \
             got: {script}"
        );
        assert!(
            !script.contains("--output /dev/null"),
            "the response body must not be discarded, the probe needs to inspect it: {script}"
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
