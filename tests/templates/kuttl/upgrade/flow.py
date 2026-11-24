#!/usr/bin/env python3
# This script is used to create a process group, run it for a couple of seconds, stop it and then
# query the number of queued flow files ("run"), or to just query the number of queued flow files
# of an already existing process group ("query").
#
# The informational output is written to stderr so that stdout only contains the number of queued
# flow files. This way the output of two invocations can be compared with diff.

import argparse
import os
import sys
import time
from dataclasses import dataclass

import nipyapi
import urllib3
from nipyapi.canvas import (
    get_root_pg_id,
    list_all_controllers,
    recurse_flow,
    schedule_controller,
    schedule_process_group,
)
from nipyapi.nifi.api_client import ApiClient
from nipyapi.security import service_login

ENDPOINT = f"https://test-nifi-node-default-0.test-nifi-node-default-headless.{os.environ['NAMESPACE']}.svc.cluster.local:8443"
USERNAME = "admin"
PASSWORD = "supersecretpassword"  # Can we use open("/nifi-users/admin").read() here?
FLOWFILE = "/tmp/generate-and-log-flowfiles.json"
CONNECTION_NAME = "output-connection"


@dataclass
class Error:
    message: str


urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)


def schedule(pg_id: str, toggle: bool) -> bool:
    for controller in list_all_controllers(pg_id):
        schedule_controller(controller, scheduled=toggle)

    return schedule_process_group(pg_id, scheduled=toggle)


def flow_files_queued(connection_name: str) -> int | Error:
    """Returns the number of queued flow files for the given connection"""
    process_groups = recurse_flow().process_group_flow.flow.process_groups

    # All flow files must be queued in a single process group, otherwise comparing the counts
    # before and after the upgrade is meaningless. More than one process group means that the
    # upload ran twice, which happens when kuttl retries the assertion in step 4 after the upload
    # already succeeded. Such a run cannot recover, so report it instead of returning the count of
    # an arbitrary copy.
    if len(process_groups) != 1:
        return Error(
            f"Expected exactly one process group, found {len(process_groups)}."
        )

    flow = recurse_flow(process_groups[0].id)
    connections = [
        conn
        for conn in flow.process_group_flow.flow.connections
        if conn.status.name == connection_name
    ]
    if not connections:
        return Error(f"No connection named '{connection_name}' found.")

    return connections[0].status.aggregate_snapshot.flow_files_queued


def parse_args(args: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Set up a flow and query its status.")
    subparsers = parser.add_subparsers(dest="subcommand", required=True)
    subparsers.add_parser(
        "run",
        help="Upload the process group, run it for a couple of seconds, stop it and query the number of queued flow files.",
    )
    subparsers.add_parser(
        "query",
        help="Query the number of queued flow files of the already uploaded process group.",
    )
    return parser.parse_args(args)


def login() -> None:
    nipyapi.config.nifi_config.host = f"{ENDPOINT}/nifi-api"
    nipyapi.config.nifi_config.verify_ssl = False

    print(f"Logging in as {USERNAME}", file=sys.stderr)
    service_login(username=USERNAME, password=PASSWORD)
    print("Logged in", file=sys.stderr)


def upload_process_group(pg_id: str) -> None:
    if not nipyapi.config.nifi_config.api_client:
        nipyapi.config.nifi_config.api_client = ApiClient()

    header_params = {}
    header_params["Accept"] = (
        nipyapi.config.nifi_config.api_client.select_header_accept(["application/json"])
    )
    header_params["Content-Type"] = (
        nipyapi.config.nifi_config.api_client.select_header_content_type(
            ["multipart/form-data"]
        )
    )

    print("Uploading process group", file=sys.stderr)
    nipyapi.config.nifi_config.api_client.call_api(
        "/process-groups/{pg_id}/process-groups/upload",
        "POST",
        path_params={"pg_id": pg_id},
        header_params=header_params,
        _return_http_data_only=True,
        post_params=[
            ("id", pg_id),
            ("groupName", "Upgrade Test"),
            ("positionX", 100),
            ("positionY", 10),
            ("clientId", nipyapi.nifi.FlowApi().generate_client_id()),
        ],
        files={"file": FLOWFILE},
        auth_settings=["tokenAuth"],
    )
    print("Process group uploaded", file=sys.stderr)


def main():
    args = parse_args(sys.argv[1:])

    login()

    if args.subcommand == "run":
        pg_id = get_root_pg_id()
        print(f"Got root process group id: {pg_id}", file=sys.stderr)

        upload_process_group(pg_id)

        schedule(pg_id, True)  # Start the flow
        time.sleep(5)  # Let the flow run for 5 seconds
        schedule(pg_id, False)  # Stop the flow

    queued = flow_files_queued(CONNECTION_NAME)
    if isinstance(queued, Error):
        print(queued.message, file=sys.stderr)
        sys.exit(1)

    print(queued)


if __name__ == "__main__":
    main()
