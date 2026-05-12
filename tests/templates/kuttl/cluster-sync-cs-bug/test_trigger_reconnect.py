#!/usr/bin/env python3
"""
Trigger the cluster-sync CS bug via disconnect/reconnect.

BUG SUMMARY
-----------
StandardVersionedComponentSynchronizer.synchronizeControllerServices calls
updateControllerService (which calls setProperties) on ENABLED controller
services without disabling them first. It relies on a separate outer
"stop affected components" pass having already disabled them, but the outer
and inner passes use different diffs and can disagree.

Specific trigger: a COMPONENT_ADDED diff on a controller service.
The outer AffectedComponentSet.addAffectedComponents short-circuits for any
non-connection COMPONENT_ADDED (line 355), so the CS is never disabled.
The inner sync's getVersionedControllerService(group, identifier) searches
ancestors, finds the root CS (ENABLED), sees different properties, marks it
for update. updateControllerService is called on the ENABLED CS ->
verifyModifiable() -> IllegalStateException -> FlowSynchronizationException.

TRIGGER SEQUENCE
----------------
1. Cluster starts with flow-trigger: root CS + child-group CS (same identifier,
   different properties). Cold start is safe (empty controller, CSes added fresh).
2. Disconnect a non-coordinator node from the cluster (node stays running,
   CSes stay ENABLED).
3. On the disconnected node, delete the child-group CS (making the node's flow
   equivalent to flow-base).
4. Reconnect the node.
5. The coordinator proposes flow-trigger to the reconnecting node.
   Diff: COMPONENT_ADDED for child-group CS.
6. Bug fires: outer pass skips the CS, inner pass tries to update ENABLED CS.

EXPECTED RESULTS
----------------
- With the bug present: markers appear in logs, test FAILS.
- With the bug patched: no markers, cluster stays stable, test PASSES.

MANUAL REPRODUCTION
-------------------
Run the KUTTL test with --skip-delete to keep the namespace:

  ./scripts/run-tests --test-suite nightly --test cluster-sync-cs-bug --skip-delete

Then exec into the test pod (for curl commands):

  kubectl exec -it -n <NAMESPACE> test-nifi-0 -- bash

Or port-forward to NiFi pods for browser UI access:

  kubectl port-forward -n <NAMESPACE> nifi-node-default-0 8443:8443
  # then open https://localhost:8443/nifi/ (accept self-signed cert)
  # login: admin / admin

The equivalent curl and NiFi UI steps are printed during the test run.
Lines starting with "  MANUAL:" are curl commands to run from the test pod.
Lines starting with "  NIFI-UI:" are steps to do in the browser.
"""

import argparse
import io
import json
import subprocess
import sys
from time import sleep, time

import requests
import urllib3

urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)


class TeeWriter:
    """Write to both the original stdout and the container's log stream.

    kubectl exec captures our stdout but doesn't feed it into the container's
    log stream (/proc/1/fd/1), so it won't show up in kubectl logs or k9s.
    This duplicates all output to both destinations.
    """

    def __init__(self, *streams):
        self.streams = streams

    def write(self, data):
        for s in self.streams:
            try:
                s.write(data)
                s.flush()
            except Exception:
                pass

    def flush(self):
        for s in self.streams:
            try:
                s.flush()
            except Exception:
                pass


def setup_tee():
    try:
        container_stdout = io.open("/proc/1/fd/1", "w")
        sys.stdout = TeeWriter(sys.__stdout__, container_stdout)
        sys.stderr = TeeWriter(sys.__stderr__, container_stdout)
    except Exception:
        pass


NIFI_PODS = ["nifi-node-default-0", "nifi-node-default-1"]
BUG_MARKERS = [
    "Cannot modify configuration of",
    "currently not disabled",
    "FlowSynchronizationException",
]


def nifi_host(pod_name, namespace):
    return (
        f"https://{pod_name}.nifi-node-default-headless"
        f".{namespace}.svc.cluster.local:8443"
    )


def headless_svc(namespace):
    return f"nifi-node-default-headless.{namespace}.svc.cluster.local"


def get_token(host, username, password):
    resp = requests.post(
        f"{host}/nifi-api/access/token",
        headers={"Content-Type": "application/x-www-form-urlencoded; charset=UTF-8"},
        data={"username": username, "password": password},
        verify=False,
        timeout=10,
    )
    if not resp.ok:
        raise RuntimeError(f"Auth failed on {host}: {resp.status_code}: {resp.text}")
    return "Bearer " + resp.text


def manual_auth(host):
    print(
        f"  MANUAL: TOKEN=$(curl -ks -X POST "
        f"-H 'Content-Type: application/x-www-form-urlencoded; charset=UTF-8' "
        f"-d 'username=admin&password=admin' "
        f"'{host}/nifi-api/access/token')"
    )


def manual_get(host, path, jq_filter):
    print(
        f'  MANUAL: curl -ks -H "Authorization: Bearer $TOKEN" '
        f"'{host}{path}' | jq '{jq_filter}'"
    )


def manual_put(host, path, body, jq_filter="."):
    print(
        f"  MANUAL: curl -ks -X PUT "
        f'-H "Authorization: Bearer $TOKEN" '
        f"-H 'Content-Type: application/json' "
        f"-d '{json.dumps(body)}' "
        f"'{host}{path}' | jq '{jq_filter}'"
    )


def manual_delete(host, path_with_params):
    print(
        f"  MANUAL: curl -ks -X DELETE "
        f'-H "Authorization: Bearer $TOKEN" '
        f"'{host}{path_with_params}'"
    )


def api_get(host, path, headers):
    resp = requests.get(f"{host}{path}", headers=headers, verify=False, timeout=10)
    resp.raise_for_status()
    return resp.json()


def api_put(host, path, headers, data):
    resp = requests.put(
        f"{host}{path}",
        headers=headers,
        json=data,
        verify=False,
        timeout=10,
    )
    resp.raise_for_status()
    return resp.json()


def api_delete(host, path, headers, params=None):
    resp = requests.delete(
        f"{host}{path}",
        headers=headers,
        params=params,
        verify=False,
        timeout=10,
    )
    resp.raise_for_status()


def get_cluster(host, headers):
    try:
        return api_get(host, "/nifi-api/controller/cluster", headers)
    except Exception:
        return None


def pod_for_address(address):
    for pod in NIFI_PODS:
        if address.startswith(f"{pod}."):
            return pod
    return None


def find_coordinator_and_victim(namespace, username, password):
    print("  Querying each pod to find the cluster coordinator...")
    for pod in NIFI_PODS:
        host = nifi_host(pod, namespace)
        try:
            token = get_token(host, username, password)
            headers = {"Authorization": token}
            print(f"\n  MANUAL: # Authenticate to {pod}")
            manual_auth(host)
            manual_get(
                host,
                "/nifi-api/controller/cluster",
                ".cluster.nodes[] | {address, status, roles, nodeId}",
            )

            cluster = get_cluster(host, headers)
            if cluster is None:
                print(f"  {pod}: cluster API returned None, trying next pod")
                continue

            nodes = cluster["cluster"]["nodes"]
            print(f"  {pod}: sees {len(nodes)} nodes in cluster:")
            for node in nodes:
                roles = ", ".join(node.get("roles", [])) or "none"
                print(
                    f"    - {node['address']} status={node['status']} "
                    f"roles=[{roles}] nodeId={node['nodeId']}"
                )

            coordinator = victim = None
            for node in nodes:
                if "Cluster Coordinator" in node.get("roles", []):
                    coordinator = node
                else:
                    victim = node

            if coordinator and victim:
                coord_pod = pod_for_address(coordinator["address"])
                if coord_pod is None:
                    print(
                        f"  Could not match coordinator address "
                        f"'{coordinator['address']}' to any known pod"
                    )
                    continue
                coord_host = nifi_host(coord_pod, namespace)
                coord_token = get_token(coord_host, username, password)
                coord_headers = {"Authorization": coord_token}
                return coordinator, victim, coord_host, coord_headers
        except Exception as e:
            print(f"  Could not reach {pod}: {e}")
    return None, None, None, None


def find_child_group_cs(host, headers):
    root = api_get(host, "/nifi-api/process-groups/root", headers)
    root_id = root["id"]
    print(f"  Root process group id: {root_id}")

    pgs = api_get(host, f"/nifi-api/process-groups/{root_id}/process-groups", headers)
    if not pgs["processGroups"]:
        print("  No child process groups found")
        return None, None, None

    child_pg = pgs["processGroups"][0]
    child_pg_id = child_pg["id"]
    print(
        f"  Child process group: '{child_pg['component']['name']}' (id={child_pg_id})"
    )

    manual_get(
        host,
        f"/nifi-api/flow/process-groups/{child_pg_id}/controller-services",
        ".controllerServices[] | {id, name: .component.name, "
        "state: .component.state, properties: .component.properties, "
        "revision}",
    )

    css = api_get(
        host,
        f"/nifi-api/flow/process-groups/{child_pg_id}/controller-services",
        headers,
    )
    if not css["controllerServices"]:
        print("  No controller services in child group")
        return child_pg_id, None, None

    cs = css["controllerServices"][0]
    cs_id = cs["id"]
    cs_revision = cs["revision"]
    cs_state = cs["component"]["state"]
    cs_props = cs["component"].get("properties", {})
    print(f"  Child-group CS: '{cs['component']['name']}' (id={cs_id})")
    print(f"    state={cs_state}")
    print(f"    properties={json.dumps(cs_props)}")
    print(f"    revision={json.dumps(cs_revision)}")
    return child_pg_id, cs_id, cs_revision


def wait_for_node_status(host, headers, node_id, target, timeout_seconds=60):
    deadline = time() + timeout_seconds
    while time() < deadline:
        cluster = get_cluster(host, headers)
        if cluster:
            for node in cluster["cluster"]["nodes"]:
                if node["nodeId"] == node_id and node["status"] == target:
                    return True
        sleep(3)
    return False


def check_pod_logs_for_bug(namespace):
    print("  Searching for bug markers in NiFi container logs...")
    print(f"  Bug markers: {BUG_MARKERS}")
    found_any = False
    for pod in NIFI_PODS:
        print(
            f"\n  MANUAL: kubectl logs -n {namespace} {pod} -c nifi --tail=2000 "
            "| grep -E 'Cannot modify|currently not disabled|FlowSynchronization'"
        )
        result = subprocess.run(
            ["kubectl", "logs", "-n", namespace, pod, "-c", "nifi", "--tail=2000"],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            print(f"  WARN: Could not read logs from {pod}")
            continue
        found = [m for m in BUG_MARKERS if m in result.stdout]
        if found:
            print(f"  {pod}: BUG MARKERS FOUND: {found}")
            for marker in found:
                for line in result.stdout.splitlines():
                    if marker in line:
                        print(f"    > {line.strip()[:200]}")
                        break
            found_any = True
        else:
            print(f"  {pod}: clean (no bug markers)")
    return found_any


def main():
    setup_tee()

    parser = argparse.ArgumentParser(
        description="Trigger NiFi cluster-sync CS bug via disconnect/reconnect",
    )
    parser.add_argument("-u", "--user", default="admin")
    parser.add_argument("-p", "--password", default="admin")
    parser.add_argument("-n", "--namespace", required=True)
    parser.add_argument("-c", "--count", type=int, default=2)
    parser.add_argument("--stable-seconds", type=int, default=30)
    parser.add_argument("--timeout-seconds", type=int, default=180)
    args = parser.parse_args()

    svc = headless_svc(args.namespace)
    ns = args.namespace

    print("=" * 72)
    print("CLUSTER-SYNC CS BUG TRIGGER TEST")
    print("=" * 72)
    print()
    print("This test reproduces a bug in NiFi's StandardVersionedComponent-")
    print("Synchronizer where synchronizeControllerServices calls setProperties")
    print("on an ENABLED controller service, causing IllegalStateException.")
    print()
    print("The trigger is a cluster reconnection where the reconnecting node's")
    print("flow differs from the coordinator's by a missing child-group CS.")
    print("The sync diff sees COMPONENT_ADDED, but the outer 'stop affected")
    print("components' pass skips COMPONENT_ADDED for non-connections, so the")
    print("root CS is never disabled before the inner pass tries to update it.")
    print()
    print(f"Namespace: {ns}")
    print(f"Expected pods: {NIFI_PODS}")
    print(f"Headless service: {svc}")
    print()
    print("To reproduce manually, run with --skip-delete then exec into the")
    print("test pod or port-forward to NiFi pods:")
    print(f"  kubectl exec -it -n {ns} test-nifi-0 -- bash")
    print(f"  kubectl port-forward -n {ns} nifi-node-default-0 8443:8443")
    print("  # then open https://localhost:8443/nifi/ — login: admin / admin")
    print()

    # --- Step 1: Find coordinator and victim ---
    print("=" * 72)
    print("STEP 1: Identify cluster coordinator and victim node")
    print("=" * 72)
    print()
    print("We need to find which pod is the Cluster Coordinator and which is")
    print("the non-coordinator 'victim' that we will disconnect.")
    print("The victim MUST be a non-coordinator node because disconnecting the")
    print("coordinator would trigger a coordinator election.")
    print()
    print("  NIFI-UI: Hamburger menu (top-right) -> Cluster")
    print("  NIFI-UI: The 'Cluster Coordinator' role is shown next to each node.")
    print()

    coordinator, victim, coord_host, coord_headers = find_coordinator_and_victim(
        ns,
        args.user,
        args.password,
    )
    if coordinator is None:
        print("\nFAIL: Could not find coordinator and victim")
        sys.exit(1)

    victim_node_id = victim["nodeId"]
    victim_addr = victim["address"]
    victim_pod = pod_for_address(victim_addr)
    coord_pod = pod_for_address(coordinator["address"])

    print("\n  Result:")
    print(f"    Coordinator: {coordinator['address']} (pod={coord_pod})")
    print(f"    Victim:      {victim_addr} (pod={victim_pod}, nodeId={victim_node_id})")

    if victim_pod is None:
        print(f"\nFAIL: Could not match victim address '{victim_addr}' to a pod")
        sys.exit(1)
    victim_host = nifi_host(victim_pod, ns)

    # --- Step 2: Verify child-group CS exists on coordinator ---
    print()
    print("=" * 72)
    print("STEP 2: Verify child-group CS exists on coordinator")
    print("=" * 72)
    print()
    print("The coordinator's flow should have a StandardHttpContextMap CS in")
    print("the child group (from flow-trigger.json). This is the CS that the")
    print("reconnecting node will NOT have, creating the COMPONENT_ADDED diff.")
    print()
    print("  NIFI-UI: On the canvas, double-click the 'ChildGroup' process group.")
    print(
        "  NIFI-UI: Right-click on empty canvas -> Configure -> Controller Services tab."
    )
    print("  NIFI-UI: You should see 'StandardHttpContextMap' with state ENABLED")
    print("  NIFI-UI: and 'Request Expiration' = '2 min'.")
    print("  NIFI-UI: Navigate back up (breadcrumb) and check root-level CS too:")
    print("  NIFI-UI: right-click canvas -> Configure -> Controller Services.")
    print("  NIFI-UI: Root CS should have 'Request Expiration' = '1 min'.")
    print("  NIFI-UI: Same CS identifier at two levels with DIFFERENT properties")
    print("  NIFI-UI: is the key to triggering the bug.")
    print()
    print(f"  Querying coordinator ({coord_pod})...")
    print("\n  MANUAL: # Authenticate to coordinator")
    manual_auth(coord_host)

    _, cs_id_coord, _ = find_child_group_cs(coord_host, coord_headers)
    if cs_id_coord is None:
        print("\nFAIL: No child-group CS found on coordinator")
        sys.exit(1)
    print(f"\n  Child-group CS confirmed on coordinator: {cs_id_coord}")

    # --- Step 3: Disconnect victim ---
    print()
    print("=" * 72)
    print("STEP 3: Disconnect victim node from cluster")
    print("=" * 72)
    print()
    print("We disconnect the victim via the NiFi REST API. The node stays")
    print("running (its NiFi JVM is still up, CSes remain ENABLED), but it")
    print("is no longer part of the cluster. This is critical: we need the")
    print("node's FlowController to be populated with ENABLED CSes so that")
    print("the bug triggers on reconnect. A cold restart would give us an")
    print("empty controller where CSes are added DISABLED (no bug).")
    print()
    print("  NIFI-UI: Hamburger menu -> Cluster -> select the victim node")
    print(f"  NIFI-UI: ({victim_pod}) -> click 'Disconnect' icon (power plug).")
    print("  NIFI-UI: Wait until the node's status shows 'DISCONNECTED'.")
    print()

    disconnect_body = {"node": {"nodeId": victim_node_id, "status": "DISCONNECTING"}}
    manual_put(
        coord_host,
        f"/nifi-api/controller/cluster/nodes/{victim_node_id}",
        disconnect_body,
        ".node | {nodeId, status, roles}",
    )
    print()

    try:
        api_put(
            coord_host,
            f"/nifi-api/controller/cluster/nodes/{victim_node_id}",
            coord_headers,
            disconnect_body,
        )
        print("  Disconnect request sent successfully")
    except Exception as e:
        print(f"\nFAIL: Disconnect request failed: {e}")
        sys.exit(1)

    print("  Waiting for victim to reach DISCONNECTED status (up to 60s)...")
    if not wait_for_node_status(
        coord_host,
        coord_headers,
        victim_node_id,
        "DISCONNECTED",
        timeout_seconds=60,
    ):
        print("\nFAIL: Victim did not reach DISCONNECTED within 60s")
        sys.exit(1)
    print("  Victim is now DISCONNECTED")

    # --- Step 4: Delete child-group CS on disconnected victim ---
    print()
    print("=" * 72)
    print("STEP 4: Delete child-group CS on the disconnected victim")
    print("=" * 72)
    print()
    print("While the victim is disconnected, we delete the child-group CS.")
    print("This makes the victim's flow equivalent to flow-base: root CS")
    print("(ENABLED) but no child-group CS. When we reconnect, the coordinator")
    print("will propose flow-trigger (which has the child-group CS), creating")
    print("a COMPONENT_ADDED diff that triggers the bug.")
    print()
    print("We must first disable the CS (NiFi won't delete an ENABLED CS),")
    print("then delete it. Both operations require disconnectedNodeAcknowledged")
    print("because we're talking to a disconnected node.")
    print()
    print("  NIFI-UI: Port-forward to the VICTIM pod (not coordinator!):")
    print(f"  NIFI-UI:   kubectl port-forward -n {ns} {victim_pod} 9443:8443")
    print("  NIFI-UI: Open https://localhost:9443/nifi/ — login: admin / admin")
    print("  NIFI-UI: (Use port 9443 to avoid conflicting with coordinator on 8443)")
    print("  NIFI-UI: You'll see a 'Disconnected' banner. This is expected.")
    print("  NIFI-UI: Double-click 'ChildGroup' -> right-click canvas -> Configure")
    print("  NIFI-UI: -> Controller Services tab.")
    print("  NIFI-UI: Click the lightning bolt on the CS -> Disable.")
    print("  NIFI-UI: Once disabled, click the trash icon to delete it.")
    print("  NIFI-UI: Navigate back to root — child group should now be empty.")
    print()

    print(f"  Authenticating to victim ({victim_pod})...")
    print("\n  MANUAL: # Authenticate to disconnected victim")
    manual_auth(victim_host)
    print()

    try:
        victim_token = get_token(victim_host, args.user, args.password)
        victim_headers = {"Authorization": victim_token}
    except Exception as e:
        print(f"\nFAIL: Could not auth to victim: {e}")
        sys.exit(1)
    print("  Authenticated to victim")

    print("\n  Querying child-group CS on victim...")
    _, victim_cs_id, victim_cs_rev = find_child_group_cs(victim_host, victim_headers)
    if victim_cs_id is None:
        print("\nFAIL: No child-group CS found on victim")
        sys.exit(1)

    # Disable the CS
    print(f"\n  Disabling CS {victim_cs_id}...")
    disable_body = {
        "revision": victim_cs_rev,
        "state": "DISABLED",
        "disconnectedNodeAcknowledged": True,
    }
    manual_put(
        victim_host,
        f"/nifi-api/controller-services/{victim_cs_id}/run-status",
        disable_body,
        ".controllerService | {id, state, revision}",
    )
    print()

    try:
        api_put(
            victim_host,
            f"/nifi-api/controller-services/{victim_cs_id}/run-status",
            victim_headers,
            disable_body,
        )
    except Exception as e:
        print(f"\nFAIL: Could not disable CS: {e}")
        sys.exit(1)

    deadline = time() + 30
    updated_rev = None
    while time() < deadline:
        try:
            cs_resp = api_get(
                victim_host,
                f"/nifi-api/controller-services/{victim_cs_id}",
                victim_headers,
            )
            state = cs_resp["component"]["state"]
            if state == "DISABLED":
                updated_rev = cs_resp["revision"]
                print(f"  CS is now DISABLED (revision={json.dumps(updated_rev)})")
                break
            else:
                print(f"  CS state: {state} (waiting for DISABLED...)")
        except Exception:
            pass
        sleep(2)

    if updated_rev is None:
        print("\nFAIL: CS did not reach DISABLED state within 30s")
        sys.exit(1)

    # Delete the CS
    print(f"\n  Deleting CS {victim_cs_id}...")
    delete_qs = (
        f"?version={updated_rev['version']}"
        f"&clientId={updated_rev.get('clientId', '')}"
        "&disconnectedNodeAcknowledged=true"
    )
    manual_delete(
        victim_host,
        f"/nifi-api/controller-services/{victim_cs_id}{delete_qs}",
    )
    print()

    try:
        api_delete(
            victim_host,
            f"/nifi-api/controller-services/{victim_cs_id}",
            victim_headers,
            params={
                "version": updated_rev["version"],
                "clientId": updated_rev.get("clientId", ""),
                "disconnectedNodeAcknowledged": "true",
            },
        )
    except Exception as e:
        print(f"\nFAIL: Could not delete CS: {e}")
        sys.exit(1)
    print("  Child-group CS deleted on victim")

    # Verify deletion
    print("\n  Verifying CS was deleted on victim...")
    _, verify_cs_id, _ = find_child_group_cs(victim_host, victim_headers)
    if verify_cs_id is None:
        print("  Confirmed: no child-group CS on victim")
    else:
        print(
            f"  NOTE: CS {verify_cs_id} visible — this is the inherited root CS, "
            "not the child-group CS (expected)"
        )

    # --- Step 5: Reconnect victim ---
    print()
    print("=" * 72)
    print("STEP 5: Reconnect victim to cluster")
    print("=" * 72)
    print()
    print("Now we reconnect the victim. The coordinator will propose its flow")
    print("(flow-trigger, with root CS + child-group CS) to the reconnecting")
    print("node. The node's current flow has only the root CS (ENABLED).")
    print()
    print("The sync diff will be:")
    print("  - Root CS: same identifier, same properties -> no change")
    print("  - Child-group CS: not present on node -> COMPONENT_ADDED")
    print()
    print("The outer AffectedComponentSet.addAffectedComponents short-circuits")
    print("for non-connection COMPONENT_ADDED, so the root CS is never disabled.")
    print("The inner getVersionedControllerService(childGroup, identifier)")
    print("searches ancestors, finds the root CS (ENABLED), sees different")
    print("properties ('2 min' vs '1 min'), calls updateControllerService on")
    print("the ENABLED CS -> verifyModifiable() -> IllegalStateException.")
    print()
    print("  NIFI-UI: On the COORDINATOR (https://localhost:8443/nifi/):")
    print("  NIFI-UI: Hamburger menu -> Cluster -> select the victim node")
    print(f"  NIFI-UI: ({victim_pod}) -> click 'Connect' icon.")
    print("  NIFI-UI: Watch the node's status — with the bug, it will flip")
    print("  NIFI-UI: between CONNECTING and DISCONNECTED repeatedly.")
    print()

    try:
        coord_token = get_token(coord_host, args.user, args.password)
        coord_headers = {"Authorization": coord_token}
    except Exception:
        pass

    reconnect_body = {"node": {"nodeId": victim_node_id, "status": "CONNECTING"}}
    print("  MANUAL: # Re-authenticate to coordinator (tokens expire)")
    manual_auth(coord_host)
    manual_put(
        coord_host,
        f"/nifi-api/controller/cluster/nodes/{victim_node_id}",
        reconnect_body,
        ".node | {nodeId, status}",
    )
    print()

    try:
        api_put(
            coord_host,
            f"/nifi-api/controller/cluster/nodes/{victim_node_id}",
            coord_headers,
            reconnect_body,
        )
        print("  Reconnect request sent")
    except Exception as e:
        print(f"\nFAIL: Reconnect request failed: {e}")
        sys.exit(1)

    # --- Step 6: Monitor cluster stability ---
    print()
    print("=" * 72)
    print(
        f"STEP 6: Monitor cluster stability "
        f"({args.stable_seconds}s stable window, "
        f"{args.timeout_seconds}s timeout)"
    )
    print("=" * 72)
    print()
    print("If the bug fires, the reconnecting node will hit an")
    print("IllegalStateException during flow sync, get disconnected by the")
    print("coordinator, then try to reconnect, hitting the same error in an")
    print("infinite loop (no backoff, no retry cap). The cluster will never")
    print("stabilise with all nodes CONNECTED.")
    print()
    print("If the bug is fixed, the sync completes, both nodes reach CONNECTED,")
    print("and stay there.")
    print()
    print("  NIFI-UI: Hamburger menu -> Cluster. Watch the status column.")
    print("  NIFI-UI: With the bug: victim flips CONNECTING -> DISCONNECTED.")
    print("  NIFI-UI: Without the bug: both nodes show CONNECTED and stay there.")
    print()
    print("  MANUAL: # Poll cluster status (re-run every few seconds)")
    manual_auth(coord_host)
    manual_get(
        coord_host,
        "/nifi-api/controller/cluster",
        '.cluster.nodes[] | {address: .address | split(".")[0], status}',
    )
    print()

    deadline = time() + args.timeout_seconds
    stable_since = None

    while time() < deadline:
        try:
            coord_token = get_token(coord_host, args.user, args.password)
            coord_headers = {"Authorization": coord_token}
            cluster = get_cluster(coord_host, coord_headers)
        except Exception:
            cluster = None

        if cluster is not None:
            nodes = cluster["cluster"]["nodes"]
            all_connected = len(nodes) == args.count and all(
                n["status"] == "CONNECTED" for n in nodes
            )
        else:
            all_connected = False

        elapsed = int(args.timeout_seconds - (deadline - time()))

        if all_connected:
            if stable_since is None:
                stable_since = time()
                print(
                    f"  [{elapsed}s] All {args.count} nodes CONNECTED — "
                    f"starting {args.stable_seconds}s stability timer"
                )
            elif time() - stable_since >= args.stable_seconds:
                stable_dur = int(time() - stable_since)
                print(
                    f"  [{elapsed}s] Cluster stable for {stable_dur}s "
                    f"(>= {args.stable_seconds}s required)"
                )
                break
        else:
            if stable_since is not None:
                print(f"  [{elapsed}s] Cluster lost stability, resetting timer")
            stable_since = None
            if cluster is not None:
                nodes = cluster["cluster"]["nodes"]
                statuses = {n["address"].split(".")[0]: n["status"] for n in nodes}
                print(f"  [{elapsed}s] Cluster: {statuses}")
            else:
                print(f"  [{elapsed}s] Cluster API unreachable")

        sleep(5)

    cluster_ok = stable_since is not None and (
        time() - stable_since >= args.stable_seconds
    )

    # --- Step 7: Check logs for bug markers ---
    print()
    print("=" * 72)
    print("STEP 7: Check NiFi logs for bug markers")
    print("=" * 72)
    print()
    print("Searching pod logs for evidence of the bug:")
    print("  - 'Cannot modify configuration of' (from verifyModifiable)")
    print("  - 'currently not disabled' (the reason string)")
    print("  - 'FlowSynchronizationException' (the wrapper exception)")
    print()

    bug_found = check_pod_logs_for_bug(ns)

    # --- Result ---
    print()
    print("=" * 72)
    print("RESULT")
    print("=" * 72)
    print()

    if not cluster_ok and bug_found:
        print("FAIL: Cluster-sync CS bug detected — node stuck in flap loop.")
        print()
        print("The FlowSynchronizationException was thrown because")
        print("synchronizeControllerServices tried to update an ENABLED CS.")
        print("The node could not rejoin the cluster within the timeout.")
        print("This confirms the bug is present in this NiFi version.")
        sys.exit(1)

    if not cluster_ok and not bug_found:
        print("FAIL: Cluster did not stabilise, but no bug markers found.")
        print()
        print(
            "The cluster never had all nodes CONNECTED for "
            f"{args.stable_seconds} consecutive seconds within "
            f"{args.timeout_seconds}s, but no FlowSynchronizationException"
        )
        print("was detected. Check the logs manually:")
        for pod in NIFI_PODS:
            print(f"  kubectl logs -n {ns} {pod} -c nifi --tail=500")
        sys.exit(1)

    if cluster_ok and bug_found:
        print("FAIL: Cluster stabilised but bug markers found in logs.")
        print()
        print("The FlowSynchronizationException was thrown at least once")
        print("during reconnection, but the node eventually recovered")
        print("(possibly via manual intervention or transient timing).")
        print("The bug is still present in this NiFi version.")
        sys.exit(1)

    print("PASS: No bug markers found and cluster is stable.")
    print()
    print("All nodes stayed CONNECTED and no FlowSynchronizationException")
    print("was detected. The sync completed cleanly.")
    sys.exit(0)


if __name__ == "__main__":
    main()
