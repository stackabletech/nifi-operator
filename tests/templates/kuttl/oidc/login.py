# $NAMESPACE will be replaced with the namespace of the test case.

import logging
import os
import requests
import sys
from bs4 import BeautifulSoup

logging.basicConfig(
    level="DEBUG", format="%(asctime)s %(levelname)s: %(message)s", stream=sys.stdout
)

namespace = sys.argv[1]
tls = os.environ["OIDC_USE_TLS"]

session = requests.Session()

nifi = f"test-nifi-node-default.{namespace}.svc.cluster.local"
keycloak_service = f"keycloak.{namespace}.svc.cluster.local"

# Open NiFi web UI which will redirect to OIDC login
login_page = session.get(
    f"https://{nifi}:8443/nifi",
    verify=False,
    headers={"Content-type": "application/json"},
)
keycloak_base_url = (
    f"https://{keycloak_service}:8443"
    if tls == "true"
    else f"http://{keycloak_service}:8080"
)
print(login_page.url)
assert login_page.ok, "Redirection from NiFi to Keycloak failed"
#assert login_page.url.startswith(
#    f"{keycloak_base_url}/realms/test/protocol/openid-connect/auth?scope=openid+email+profile&response_type=code&redirect_uri=https%3A%2F%2F{nifi}%3A9088%2Fdruid-ext%2Fdruid-pac4j%2Fcallback&state="
#), "Redirection to Keycloak expected"

# Login to keycloak with test user
login_page_html = BeautifulSoup(login_page.text, "html.parser")
authenticate_url = login_page_html.form["action"]
welcome_page = session.post(
    authenticate_url, data={"username": "nifi-admin", "password": "nifi-admin"}
)
print(welcome_page.url)
assert welcome_page.ok, "Login failed"
assert (
    welcome_page.url == f"https://{nifi}:8443/nifi"
), "Redirection to the NiFi web UI expected"
