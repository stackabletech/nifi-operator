#!/usr/bin/env python
import requests
import json
import argparse
import urllib3
from time import sleep


def get_token(nifi_host, username, password):
    nifi_headers = {
        "content-type": "application/x-www-form-urlencoded; charset=UTF-8",
    }
    data = {"username": username, "password": password}

    # TODO: handle actual errors when connecting properly
    nifi_url = nifi_host + "/nifi-api/access/token"
    response = requests.post(
        nifi_url, headers=nifi_headers, data=data, verify=False
    )  # , cert='./tmp/cacert.pem')

    if response.ok:
        nifi_token = response.content.decode("utf-8")
        return "Bearer " + nifi_token
    else:
        print(f"Failed to get token: {response.status_code}: {response.content}")
        exit(-1)


if __name__ == "__main__":
    # Construct an argument parser
    all_args = argparse.ArgumentParser()

    # Add arguments to the parser
    all_args.add_argument("-u", "--user", required=True, help="Username to connect as")
    all_args.add_argument(
        "-p", "--password", required=True, help="Password for the user"
    )
    all_args.add_argument(
        "-n", "--namespace", required=True, help="Namespace the test is running in"
    )
    all_args.add_argument(
        "-c", "--count", required=True, help="The expected number of Nodes"
    )
    args = vars(all_args.parse_args())

    # disable warnings as we have specified non-verified https connections
    urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

    host = f"https://nifi-node-default-1.nifi-node-default-headless.{args['namespace']}.svc.cluster.local:8443"
    token = get_token(host, args["user"], args["password"])
    headers = {"Authorization": token}
    node_count = int(args["count"])

    x = 0
    while x < 15:
        url = host + "/nifi-api/controller/cluster"
        cluster = requests.get(
            url, headers=headers, verify=False
        )  # , cert='/tmp/cacert.pem')
        if cluster.status_code != 200:
            print("Waiting for cluster...")
        else:
            cluster_data = json.loads(cluster.content.decode("utf-8"))
            nodes = cluster_data["cluster"]["nodes"]
            if len(nodes) != node_count:
                print(
                    f"Cluster should have {node_count} nodes at this stage, but has: {len(nodes)}"
                )
            else:
                connected = True
                for node in nodes:
                    if node["status"] != "CONNECTED":
                        print(
                            f"Node {node['nodeId']} is in state {node['status']} but should have been CONNECTED"
                        )
                        connected = False
                if connected:
                    print("Test succeeded!")
                    exit(0)
            print("Retrying...")
        x = x + 1
        sleep(10)

    print("Test failed")
    exit(-1)
