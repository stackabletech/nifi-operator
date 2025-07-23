#!/usr/bin/env bash
set -euo pipefail

# DO NOT EDIT THE RENDERED SCRIPT
# Instead, update the j2 template, and regenerate it for dev with `make render-docs`.

# The getting started guide script
# It uses tagged regions which are included in the documentation
# https://docs.asciidoctor.org/asciidoc/latest/directives/include-tagged-regions/
#
# There are two variants to go through the guide - using stackablectl or helm
# The script takes either 'stackablectl' or 'helm' as an argument
#
# The script can be run as a test as well, to make sure that the tutorial works
# It includes some assertions throughout, and at the end especially.

if [ $# -eq 0 ]
then
  echo "Installation method argument ('helm' or 'stackablectl') required."
  exit 1
fi

cd "$(dirname "$0")"

case "$1" in
"helm")
echo "Installing Operators with Helm"
# tag::helm-install-operators[]
helm install --wait commons-operator oci://oci.stackable.tech/sdp-charts/commons-operator --version 25.7.0
helm install --wait secret-operator oci://oci.stackable.tech/sdp-charts/secret-operator --version 25.7.0
helm install --wait listener-operator oci://oci.stackable.tech/sdp-charts/listener-operator --version 25.7.0
helm install --wait nifi-operator oci://oci.stackable.tech/sdp-charts/nifi-operator --version 25.7.0
# end::helm-install-operators[]
;;
"stackablectl")
echo "installing Operators with stackablectl"
# tag::stackablectl-install-operators[]
stackablectl operator install \
  commons=25.7.0 \
  secret=25.7.0 \
  listener=25.7.0 \
  nifi=25.7.0
# end::stackablectl-install-operators[]
;;
*)
echo "Need to provide 'helm' or 'stackablectl' as an argument for which installation method to use!"
exit 1
;;
esac

echo "Create NiFi admin credentials"
# tag::install-nifi-credentials[]
kubectl apply -f - <<EOF
---
apiVersion: v1
kind: Secret
metadata:
  name: simple-admin-credentials
stringData:
  admin: admin
---
apiVersion: authentication.stackable.tech/v1alpha1
kind: AuthenticationClass
metadata:
  name: simple-nifi-users
spec:
  provider:
    static:
      userCredentialsSecret:
        name: simple-admin-credentials
EOF
# end::install-nifi-credentials[]

echo "Create a NiFi instance"
# tag::install-nifi[]
kubectl apply -f - <<EOF
---
apiVersion: nifi.stackable.tech/v1alpha1
kind: NifiCluster
metadata:
  name: simple-nifi
spec:
  image:
    productVersion: 2.4.0
  clusterConfig:
    authentication:
      - authenticationClass: simple-nifi-users
    sensitiveProperties:
      keySecret: nifi-sensitive-property-key
      autoGenerate: true
  nodes:
    roleConfig:
      listenerClass: external-unstable
    roleGroups:
      default:
        replicas: 1
EOF
# end::install-nifi[]

echo "Awaiting NiFi rollout finish"
# tag::wait-nifi-rollout[]
kubectl wait --for=condition=available --timeout=20m nificluster/simple-nifi
# end::wait-nifi-rollout[]

case "$1" in
"helm")
echo "Getting the NiFi URL with kubectl"

# tag::get-nifi-url[]
nifi_url=$(kubectl get listener simple-nifi-node -o 'jsonpath=https://{.status.ingressAddresses[0].address}:{.status.ingressAddresses[0].ports.https}') && \
echo "NiFi URL: $nifi_url"
# end::get-nifi-url[]
;;
"stackablectl")
echo "Getting NiFi URL with stackablectl ..."
# tag::stackablectl-nifi-url[]
nifi_url=$(stackablectl stacklet ls -o json | jq --raw-output '.[] | select(.name == "simple-nifi") | .endpoints["node-https"]')
# end::stackablectl-nifi-url[]
echo "NiFi URL: $nifi_url"
;;
*)
echo "Need to provide 'helm' or 'stackablectl' as an argument for which installation method to use!"
exit 1
;;
esac

echo "Starting nifi tests"
chmod +x ./test-nifi.sh
./test-nifi.sh "$nifi_url"
