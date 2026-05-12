#!/usr/bin/env python3
"""Check that a NiFi cluster has the expected number of CONNECTED nodes."""

import argparse
import sys
from time import sleep

import requests
import urllib3

urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)


def get_token(host, username, password):
    resp = requests.post(
        f"{host}/nifi-api/access/token",
        headers={"Content-Type": "application/x-www-form-urlencoded; charset=UTF-8"},
        data={"username": username, "password": password},
        verify=False,
    )
    if not resp.ok:
        print(f"Failed to get token: {resp.status_code}: {resp.text}")
        sys.exit(1)
    return "Bearer " + resp.text


def get_cluster(host, headers):
    resp = requests.get(
        f"{host}/nifi-api/controller/cluster", headers=headers, verify=False
    )
    if resp.status_code != 200:
        return None
    return resp.json()


def check_cluster(host, headers, expected_count):
    cluster = get_cluster(host, headers)
    if cluster is None:
        return False, "Could not reach cluster API"
    nodes = cluster["cluster"]["nodes"]
    if len(nodes) != expected_count:
        return False, f"Expected {expected_count} nodes, got {len(nodes)}"
    for node in nodes:
        if node["status"] != "CONNECTED":
            return (
                False,
                f"Node {node['nodeId']} status={node['status']}, expected CONNECTED",
            )
    return True, "All nodes CONNECTED"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("-u", "--user", default="admin")
    parser.add_argument("-p", "--password", default="admin")
    parser.add_argument("-n", "--namespace", required=True)
    parser.add_argument(
        "-c", "--count", type=int, required=True, help="Expected node count"
    )
    parser.add_argument("--retries", type=int, default=15)
    parser.add_argument("--interval", type=int, default=10)
    args = parser.parse_args()

    host = (
        f"https://nifi-node-default-0.nifi-node-default-headless"
        f".{args.namespace}.svc.cluster.local:8443"
    )
    token = get_token(host, args.user, args.password)
    headers = {"Authorization": token}

    for attempt in range(args.retries):
        ok, msg = check_cluster(host, headers, args.count)
        if ok:
            print(f"OK: {msg}")
            sys.exit(0)
        print(f"Attempt {attempt + 1}/{args.retries}: {msg}")
        sleep(args.interval)

    print("FAIL: cluster did not reach healthy state in time")
    sys.exit(1)


if __name__ == "__main__":
    main()
