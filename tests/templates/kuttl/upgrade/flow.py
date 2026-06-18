#!/usr/bin/env python3
# This script is used to create a process group, run it for a couple of seconds, stop it and then
# query the number of queued flow files.

import nipyapi
from nipyapi.canvas import (
    get_root_pg_id,
    schedule_process_group,
    list_all_controllers,
    schedule_controller,
    recurse_flow,
)
from nipyapi.security import service_login
from nipyapi.nifi.api_client import ApiClient

from typing import Union
from dataclasses import dataclass

import os
import time
import urllib3

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


def flow_files_queued(connection_name: str) -> Union[str, Error]:
    """Returns the current input record number for the given component"""
    flow = recurse_flow()

    if flow.process_group_flow.flow.process_groups:
        inner_flow = recurse_flow(flow.process_group_flow.flow.process_groups[0].id)
        conn = [
            conn
            for conn in inner_flow.process_group_flow.flow.connections
            if conn.status.name == connection_name
        ]
        if conn:
            return conn[0].status.aggregate_snapshot.flow_files_queued
        else:
            return Error(f"No connection named '{connection_name}' found.")
    else:
        return Error(f"No connection named '{connection_name}' found.")


def main():
    nipyapi.config.nifi_config.host = f"{ENDPOINT}/nifi-api"
    nipyapi.config.nifi_config.verify_ssl = False

    print(f"Logging in as {USERNAME}")
    service_login(username=USERNAME, password=PASSWORD)
    print("Logged in")

    pg_id = get_root_pg_id()
    print(f"Got root process group id: {pg_id}")

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

    print("Uploading process group")
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
    print("Process group uploaded")

    schedule(pg_id, True)  # Start the flow
    time.sleep(5)  # Let the flow run for 5 seconds
    schedule(pg_id, False)  # Stop the flow

    print(flow_files_queued(CONNECTION_NAME))


if __name__ == "__main__":
    main()
