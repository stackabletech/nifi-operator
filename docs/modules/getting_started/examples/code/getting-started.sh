#!/usr/bin/env bash
set -euo pipefail

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

case "$1" in
"helm")
echo "Adding 'stackable-dev' Helm Chart repository"
# tag::helm-add-repo[]
helm repo add stackable-dev https://repo.stackable.tech/repository/helm-dev/
# end::helm-add-repo[]
echo "Installing Operators with Helm"
# tag::helm-install-operators[]
helm install --wait commons-operator stackable-dev/commons-operator --version 0.3.0-nightly
helm install --wait secret-operator stackable-dev/secret-operator --version 0.6.0-nightly
helm install --wait zookeeper-operator stackable-dev/zookeeper-operator --version 0.11.0-nightly
helm install --wait nifi-operator stackable-dev/nifi-operator --version 0.7.0-nightly
# end::helm-install-operators[]
;;
"stackablectl")
echo "installing Operators with stackablectl"
# tag::stackablectl-install-operators[]
stackablectl operator install \
  commons=0.3.0-nightly \
  secret=0.6.0-nightly \
  zookeeper=0.11.0-nightly \
  nifi=0.7.0-nightly
# end::stackablectl-install-operators[]
;;
*)
echo "Need to provide 'helm' or 'stackablectl' as an argument for which installation method to use!"
exit 1
;;
esac

echo "Installing ZooKeeper from zookeeper.yaml"
# tag::install-zookeeper[]
kubectl apply -f zookeeper.yaml
# end::install-zookeeper[]

echo "Installing ZNode from nifi-znode.yaml"
# tag::install-znode[]
kubectl apply -f nifi-znode.yaml
# end::install-znode[]

sleep 5

echo "Awaiting ZooKeeper rollout finish"
# tag::watch-zookeeper-rollout[]
kubectl rollout status --watch statefulset/simple-zk-server-default
# end::watch-zookeeper-rollout[]

echo "Install the NiFi admin credentials from nifi-admin-credentials.yaml"
# tag::install-nifi[]
kubectl apply -f nifi-admin-credentials.yaml
# end::install-nifi[]

echo "Install NiFiCluster from nifi.yaml"
# tag::install-nifi[]
kubectl apply -f nifi.yaml
# end::install-nifi[]

sleep 5

echo "Starting nifi tests"
# tag::nifi-tests[]
chmod +x ./test-nifi.sh
./test-nifi.sh
# end::nifi-tests[]
