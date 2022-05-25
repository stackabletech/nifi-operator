#!/usr/bin/env python
import requests
import json
import argparse


def get_token(username, password, namespace):
    headers = {
        'content-type': 'application/x-www-form-urlencoded; charset=UTF-8',
    }

    data = {'username': username, 'password': password}

    # TODO: handle actual errors when connecting properly
    url = 'https://test-nifi-node-default-1.test-nifi-node-default.' + namespace + '.svc.cluster.local:8443/nifi-api/access/token'
    response = requests.post(url, headers=headers, data=data, verify=False)#, cert='./tmp/cacert.pem')

    if response.ok:
        token = response.content.decode('utf-8')
        return "Bearer " + token
    else:
        print("Failed to get token: ", response.status_code, " - ", response.content)
        exit(-1)


if __name__ == '__main__':
    # Construct an argument parser
    all_args = argparse.ArgumentParser()

    # Add arguments to the parser
    all_args.add_argument("-u", "--user", required=True,
                          help="Username to connect as")
    all_args.add_argument("-p", "--password", required=True,
                          help="Password for the user")
    all_args.add_argument("-n", "--namespace", required=True,
                          help="Namespace the test is running in")
    args = vars(all_args.parse_args())

    token = get_token(args['user'], args['password'], args['namespace'])

    headers = {'Authorization': token}
    url = 'https://test-nifi-node-default-1.test-nifi-node-default.' + args['namespace'] + '.svc.cluster.local:8443/nifi-api/controller/cluster'
    cluster = requests.get(url, headers=headers, verify=False)#, cert='/tmp/cacert.pem')

    if cluster.ok:
        cluster_data = json.loads(cluster.content.decode('utf-8'))
    else:
        print("Failed to get cluster data: ", cluster.status_code, " - ", cluster.content)

    nodes = cluster_data['cluster']['nodes']

    if len(nodes) != 3:
        print("Cluster should have 2 nodes at this stage, but has: ", len(nodes))
        exit(-1)

    for node in nodes:
        if node['status'] != "CONNECTED":
            print('Node ', node['nodeId'], ' is in state ', node['status'], ' but should have been CONNECTED')
            exit(-1)

    print("Test succeeded!")
