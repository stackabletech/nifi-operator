import logging
import os
import requests
import sys
import json
from bs4 import BeautifulSoup

logging.basicConfig(
    level="DEBUG", format="%(asctime)s %(levelname)s: %(message)s", stream=sys.stdout
)

namespace = os.environ["NAMESPACE"]
tls = os.environ["OIDC_USE_TLS"]
nifi_version = os.environ["NIFI_VERSION"]

session = requests.Session()

nifi = f"test-nifi-node-default-0.test-nifi-node-default.{namespace}.svc.cluster.local"
keycloak_service = f"keycloak.{namespace}.svc.cluster.local"

keycloak_base_url = (
    f"https://{keycloak_service}:8443"
    if tls == "true"
    else f"http://{keycloak_service}:8080"
)

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
    data={"username": "jane.doe", "password": "T8mn72D9"},
    verify=False,
)
assert welcome_page.ok, "Login failed"
assert (
    welcome_page.url == f"https://{nifi}:8443/nifi/"
), "Redirection to the NiFi web UI expected"
