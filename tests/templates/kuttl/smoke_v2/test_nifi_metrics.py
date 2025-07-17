#!/usr/bin/env python
import requests
import time
import argparse

if __name__ == "__main__":
    # Construct an argument parser
    all_args = argparse.ArgumentParser()

    # Add arguments to the parser
    all_args.add_argument(
        "-m",
        "--metric",
        required=False,
        default="nifi_amount_bytes_read",
        help="The name of a certain metric to check",
    )
    all_args.add_argument(
        "-n", "--namespace", required=True, help="The namespace the test is running in"
    )
    all_args.add_argument(
        "-p",
        "--port",
        required=False,
        default="8443",
        help="The port where metrics are exposed",
    )
    all_args.add_argument(
        "-t",
        "--timeout",
        required=False,
        default="120",
        help="The timeout in seconds to wait for the metrics port to be opened",
    )

    args = vars(all_args.parse_args())
    metric_name = args["metric"]
    namespace = args["namespace"]
    port = args["port"]
    timeout = int(args["timeout"])

    url = f"https://nifi-node-default-metrics.{args['namespace']}.svc.cluster.local:{port}/nifi-api/flow/metrics/prometheus"

    # wait for 'timeout' seconds
    t_end = time.time() + timeout
    while time.time() < t_end:
        try:
            response = requests.get(
                url,
                cert=("/stackable/tls/tls.crt", "/stackable/tls/tls.key"),
                verify="/stackable/tls/ca.crt",
            )
            response.raise_for_status()
            if metric_name in response.text:
                print("Test metrics succeeded!")
                exit(0)
            else:
                print(
                    f"Could not find metric [{metric_name}] in response:\n {response.text}"
                )
                time.sleep(timeout)
        except ConnectionError:
            # NewConnectionError is expected until metrics are available
            time.sleep(10)

    print("Test failed")
    exit(-1)
