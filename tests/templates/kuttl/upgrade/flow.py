#!/usr/bin/env python3
# This script is used to create a flow from a template, run it for a couple
# of seconds, stop it and then query the number of queued flow files.

import nipyapi
from nipyapi.canvas import (
    get_root_pg_id,
    schedule_process_group,
    list_all_controllers,
    schedule_controller,
    recurse_flow,
)
from nipyapi.security import service_login
from nipyapi.templates import get_template, upload_template, deploy_template
from nipyapi.nifi.models import FlowEntity

from typing import Union, List
from dataclasses import dataclass

import re
import os
import sys
import time
import argparse
import urllib3

USERNAME = "admin"
PASSWORD = "supersecretpassword"
TEMPLATE_NAME = "generate-and-log-flowfiles"
TEMPLATE_FILE = f"{TEMPLATE_NAME}.xml"
JSON_NAME = "generate-and-log-flowfiles"
JSON_FILE = f"{JSON_NAME}.json"
CONNECTION_NAME = "output-connection"


@dataclass
class Error:
    message: str


def parse_args(args: List[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Setup a flow and query the status.")
    parser.add_argument(
        "-e",
        "--endpoint",
        help="Nifi endpoint URL (must be https). As of 2022-08-29 we cant use https://nifi:8443 here because Nifi's check for the host header validity fails. The error message looks like: <h2>The request contained an invalid host header [<code>nifi:8443</code>] in the request [<code>/nifi-api</code>]. Check for request manipulation or third-party intercept.</h2>",
        required=True,
    )
    parser.add_argument(
        "-u",
        "--user",
        help="Nifi login user.",
    )
    parser.add_argument(
        "-p",
        "--password",
        help="Nifi login password.",
    )
    subparsers = parser.add_subparsers(dest="subcommand", required=True)
    run_parser = subparsers.add_parser(
        "run",
        help="Install and run a flow.",
    )
    format_parser = run_parser.add_subparsers(dest="formatcommand")
    template_parser = format_parser.add_parser(
        "template", help="Use the template format for the flow."
    )
    template_parser.add_argument(
        "template",
        help="Nifi template file.",
    )
    json_parser = format_parser.add_parser(
        "json", help="Use the json format for the flow."
    )
    json_parser.add_argument(
        "json",
        help="Nifi json file.",
    )
    subparsers.add_parser(
        "query",
        help="Query the flow.",
    )
    return parser.parse_args(args)


def login(endpoint: str, user: str, passwd: str) -> bool:
    nipyapi.config.nifi_config.host = f"{endpoint}/nifi-api"
    nipyapi.config.nifi_config.verify_ssl = False
    return service_login(username=user, password=passwd)


def flow_from_template(
    pg_id: str, template_file: str, template_name: str
) -> FlowEntity:
    upload_template(pg_id, template_file)
    template_id = get_template(template_name).id
    return deploy_template(pg_id, template_id, 200, 0)


def schedule(pg_id: str, toggle: bool) -> bool:
    for controller in list_all_controllers(pg_id):
        schedule_controller(controller, scheduled=toggle)
    return schedule_process_group(pg_id, scheduled=toggle)


def flow_files_queued(connection_name: str) -> Union[str, Error]:
    """Returns the current input record number for the given component"""
    flow = recurse_flow()
    c = [
        c
        for c in flow.process_group_flow.flow.connections
        if c.status.name == connection_name
    ]
    if c:
        return c[0].status.aggregate_snapshot.flow_files_queued
    else:
        # for nifi 2.x the flow is loaded via api into a new process group
        # we need to look for the connection in that process group instead
        if flow.process_group_flow.flow.process_groups:
            inner_flow = recurse_flow(flow.process_group_flow.flow.process_groups[0].id)
            inner_c = [
                inner_c
                for inner_c in inner_flow.process_group_flow.flow.connections
                if inner_c.status.name == connection_name
            ]
            if inner_c:
                return inner_c[0].status.aggregate_snapshot.flow_files_queued
            else:
                return Error(f"No connection named '{connection_name}' found.")
        else:
            return Error(f"No connection named '{connection_name}' found.")


def main():
    # turn this on to debug Nifi api calls. Also need to import logging
    # logging.getLogger().setLevel(logging.INFO)

    args = parse_args(sys.argv[1:])

    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

    username = args.user or USERNAME
    password = args.password or PASSWORD

    if args.subcommand == "run":
        login(args.endpoint, username, password)
        pg_id = get_root_pg_id()

        if args.formatcommand == "template":
            template_file = args.template or TEMPLATE_FILE
            template_name = re.sub(r"\..+$", "", os.path.basename(template_file))

            flow_from_template(pg_id, template_file, template_name)

        elif args.formatcommand == "json":
            json_file = args.json or JSON_FILE
            json_name = re.sub(r"\..+$", "", os.path.basename(json_file))

            if not nipyapi.config.nifi_config.api_client:
                nipyapi.config.nifi_config.api_client = nipyapi.nifi.ApiClient()

            header_params = {}
            header_params["Accept"] = (
                nipyapi.config.nifi_config.api_client.select_header_accept(
                    ["application/json"]
                )
            )
            header_params["Content-Type"] = (
                nipyapi.config.nifi_config.api_client.select_header_content_type(
                    ["multipart/form-data"]
                )
            )

            nipyapi.config.nifi_config.api_client.call_api(
                "/process-groups/{pg_id}/process-groups/upload",
                "POST",
                path_params={"pg_id": pg_id},
                header_params=header_params,
                _return_http_data_only=True,
                post_params=[
                    ("id", pg_id),
                    ("groupName", json_name),
                    ("positionX", 100),
                    ("positionY", 10),
                    ("clientId", nipyapi.nifi.FlowApi().generate_client_id()),
                ],
                files={"file": json_file},
                auth_settings=["tokenAuth"],
            )

            # get the process group id of the newly created process group to pass it to the schedule() function
            # for nifi 2.x we need to schedule the inner process group and instead of the root process group
            flow = recurse_flow()
            inner_flow_id = flow.process_group_flow.flow.process_groups[0].id
            pg_id = inner_flow_id

        schedule(pg_id, True)  # start
        time.sleep(5)  # give the flow 5 seconds to run
        schedule(pg_id, False)  # stop
        print(flow_files_queued(CONNECTION_NAME))

    elif args.subcommand == "query":
        login(args.endpoint, username, password)
        print(flow_files_queued(CONNECTION_NAME))


if __name__ == "__main__":
    main()
