import logging
import os
import requests
import sys
import time
import json
from bs4 import BeautifulSoup

logging.basicConfig(
    level="DEBUG", format="%(asctime)s %(levelname)s: %(message)s", stream=sys.stdout
)

namespace = os.environ["NAMESPACE"]
tls = os.environ["OIDC_USE_TLS"]
nifi_version = os.environ["NIFI_VERSION"]
nifi = f"test-nifi-node-default-0.test-nifi-node-default.{namespace}.svc.cluster.local"
keycloak_service = f"keycloak.{namespace}.svc.cluster.local"

keycloak_base_url = (
    f"https://{keycloak_service}:8443"
    if tls == "true"
    else f"http://{keycloak_service}:8080"
)


def login(session: requests.Session, username: str, password: str):
    # startswith instead of an exact check to
    # a) hit all 2.x versions and
    # b) to allow for custom images because `nifi_version` will contain the whole custom image string
    # e.g. 2.0.0,localhost:5000/stackable/nifi:2.0.0-stackable0.0.0-dev
    if not nifi_version.startswith("1."):
        auth_config_page = session.get(
            f"https://{nifi}:8443/nifi-api/authentication/configuration",
            verify=False,
            headers={"Content-type": "application/json"},
        )
        assert auth_config_page.ok, "Could not fetch auth config from NiFi"
        auth_config = json.loads(auth_config_page.text)
        login_url = auth_config["authenticationConfiguration"]["loginUri"]
    else:
        login_url = f"https://{nifi}:8443/nifi/login"

    # Open NiFi web UI which will redirect to OIDC login
    login_page = session.get(
        login_url,
        verify=False,
        headers={"Content-type": "application/json"},
    )

    print("actual: ", login_page.url)
    print(
        "expected: ",
        f"{keycloak_base_url}/realms/test/protocol/openid-connect/auth?response_type=code&client_id=nifi&scope=",
    )
    assert login_page.ok, "Redirection from NiFi to Keycloak failed"
    assert login_page.url.startswith(
        f"{keycloak_base_url}/realms/test/protocol/openid-connect/auth?response_type=code&client_id=nifi&scope="
    ), "Redirection to Keycloak expected"

    # Login to keycloak with test user
    login_page_html = BeautifulSoup(login_page.text, "html.parser")
    authenticate_url = login_page_html.form["action"]
    welcome_page = session.post(
        authenticate_url,
        data={"username": username, "password": password},
        verify=False,
    )
    assert welcome_page.ok, "Login failed"
    assert welcome_page.url == f"https://{nifi}:8443/nifi/", (
        "Redirection to the NiFi web UI expected"
    )
    print(f"logged in as {username}")


def get_process_group_a(session: requests.Session) -> requests.Response:
    return get_resource_with_retries(
        session, "/flow/process-groups/c9186a05-0196-1000-ffff-ffffd8474359"
    )


def get_process_group_b(session: requests.Session) -> requests.Response:
    return get_resource_with_retries(
        session, "/flow/process-groups/7e08561b-447d-3acb-b510-744d886c3ca4"
    )


def get_processor_e(session: requests.Session) -> requests.Response:
    return get_resource_with_retries(
        session, "/processors/9d95cac3-2759-3fce-9c07-71215b0fb554"
    )


def get_counters(session: requests.Session) -> requests.Response:
    return get_resource_with_retries(session, "/counters")


def get_resource_with_retries(
    session: requests.Session, resource: str
) -> requests.Response:
    retries = 0
    max_retries = 5
    while True:
        time.sleep(retries ^ 2)
        response = session.get(
            f"https://{nifi}:8443/nifi-api{resource}?uiOnly=true",
            verify=False,
        )
        # Occasionally NiFi will respond with an 409 http error
        if response.status_code == 409 and retries <= max_retries:
            print("NiFi returned HTTP 409")
            retries += 1
        else:
            return response


# alice
session = requests.Session()
login(session, "alice", "alice")

process_group_a = get_process_group_a(session)
assert process_group_a.json()["permissions"]["canRead"], (
    "Alice should be able to access process group A"
)
process_group_b = get_process_group_b(session)
assert not process_group_b.json()["permissions"]["canRead"], (
    "Alice should not be able to access process group B"
)
processor_e = get_processor_e(session)
assert processor_e.json()["permissions"]["canRead"], (
    "Alice should be able to read processor E in process group C"
)
assert not processor_e.json()["permissions"]["canWrite"], (
    "Alice should not be able to write to processor E in process group C"
)

counters = get_counters(session)
assert not counters.ok, (
    "Alice should not be able to access the global resource 'counters'"
)


# bob
session = requests.Session()
login(session, "bob", "bob")

process_group_a = get_process_group_a(session)
assert not process_group_a.json()["permissions"]["canRead"], (
    "Bob should not be able to access process group A"
)

process_group_b = get_process_group_b(session)
assert process_group_b.json()["permissions"]["canRead"], (
    "Bob should be able to access process group B"
)

processor_e = get_processor_e(session)
assert processor_e.json()["permissions"]["canRead"], (
    "Bob should be able to read processor E in process group C"
)
assert not processor_e.json()["permissions"]["canWrite"], (
    "Bob should not be able to write to processor E in process group C"
)

counters = get_counters(session)
assert not counters.ok, (
    "Bob should not be able to access the global resource 'counters'"
)


# charlie
session = requests.Session()
login(session, "charlie", "charlie")

process_group_a = get_process_group_a(session)
assert not process_group_a.json()["permissions"]["canRead"], (
    "Charlie should not be able to access process group A"
)

process_group_b = get_process_group_b(session)
assert not process_group_b.json()["permissions"]["canRead"], (
    "Charlie should not be able to access process group B"
)

processor_e = get_processor_e(session)
assert processor_e.json()["permissions"]["canRead"], (
    "Charlie should be able to read processor E in process group C"
)
assert not processor_e.json()["permissions"]["canWrite"], (
    "Charlie should not be able to write to processor E in process group C"
)

counters = get_counters(session)
assert not counters.ok, (
    "Charlie should not be able to access the global resource 'counters'"
)

# nifi-admin
session = requests.Session()
login(session, "nifi-admin", "nifi-admin")

process_group_a = get_process_group_a(session)
assert process_group_a.json()["permissions"]["canRead"], (
    "Nifi-admin should be able to access process group A"
)

process_group_b = get_process_group_b(session)
assert process_group_b.json()["permissions"]["canRead"], (
    "Nifi-admin should be able to access process group B"
)

processor_e = get_processor_e(session)
assert processor_e.json()["permissions"]["canRead"], (
    "Nifi-admin should be able to read processor E in process group C"
)
assert processor_e.json()["permissions"]["canWrite"], (
    "Nifi-admin should be able to write to processor E in process group C"
)

counters = get_counters(session)
assert counters.ok, "Nifi-admin should be able to access the global resource 'counters'"
