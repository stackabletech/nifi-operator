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

cd "$(dirname "$0")"

case "$1" in
"helm")
echo "Adding 'stackable-dev' Helm Chart repository"
# tag::helm-add-repo[]
helm repo add stackable-dev https://repo.stackable.tech/repository/helm-dev/
# end::helm-add-repo[]
echo "Updating Helm repo"
helm repo update

echo "Installing Operators with Helm"
# tag::helm-install-operators[]
helm install --wait commons-operator stackable-dev/commons-operator --version 0.0.0-dev
helm install --wait secret-operator stackable-dev/secret-operator --version 0.0.0-dev
helm install --wait zookeeper-operator stackable-dev/zookeeper-operator --version 0.0.0-dev
helm install --wait nifi-operator stackable-dev/nifi-operator --version 0.0.0-dev
# end::helm-install-operators[]
;;
"stackablectl")
echo "installing Operators with stackablectl"
# tag::stackablectl-install-operators[]
stackablectl operator install \
  commons=0.0.0-dev \
  secret=0.0.0-dev \
  zookeeper=0.0.0-dev \
  nifi=0.0.0-dev
# end::stackablectl-install-operators[]
;;
*)
echo "Need to provide 'helm' or 'stackablectl' as an argument for which installation method to use!"
exit 1
;;
esac

internal=true
if [ $# -eq 2 ]
then
  case "$2" in
  "InternalIP")
  internal=true
  ;;
  "ExternalIP")
  internal=false
  ;;
  *)
  echo "Need to provide 'InternalIP' or 'ExternalIP' as the connection type argument."
  exit 1
  ;;
  esac
fi

echo "Installing ZooKeeper"
# tag::install-zookeeper[]
kubectl apply -f - <<EOF
---
apiVersion: zookeeper.stackable.tech/v1alpha1
kind: ZookeeperCluster
metadata:
  name: simple-zk
spec:
  image:
    productVersion: 3.8.0
  servers:
    roleGroups:
      default:
        replicas: 3
EOF
# end::install-zookeeper[]

echo "Create a ZNode"
# tag::install-znode[]
kubectl apply -f - <<EOF
---
apiVersion: zookeeper.stackable.tech/v1alpha1
kind: ZookeeperZnode
metadata:
  name: simple-nifi-znode
spec:
  clusterRef:
    name: simple-zk
EOF
# end::install-znode[]

sleep 15

echo "Awaiting ZooKeeper rollout finish"
# tag::watch-zookeeper-rollout[]
kubectl rollout status --watch --timeout=5m statefulset/simple-zk-server-default
# end::watch-zookeeper-rollout[]

echo "Create NiFi admin credentials"
# tag::install-nifi-credentials[]
kubectl apply -f - <<EOF
---
apiVersion: v1
kind: Secret
metadata:
  name: nifi-admin-credentials-simple
stringData:
  username: admin
  password: admin
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
    productVersion: 1.21.0
  clusterConfig:
    authentication:
      method:
        singleUser:
          adminCredentialsSecret: nifi-admin-credentials-simple
          autoGenerate: true
    listenerClass: external-unstable
    sensitiveProperties:
      keySecret: nifi-sensitive-property-key
      autoGenerate: true
    zookeeperConfigMapName: simple-nifi-znode
  nodes:
    roleGroups:
      default:
        replicas: 2
EOF
# end::install-nifi[]

sleep 5

echo "Awaiting NiFi rollout finish"
# tag::wait-nifi-rollout[]
kubectl wait -l statefulset.kubernetes.io/pod-name=simple-nifi-node-default-0 \
--for=condition=ready pod --timeout=1200s && \
kubectl wait -l statefulset.kubernetes.io/pod-name=simple-nifi-node-default-1 \
--for=condition=ready pod --timeout=1200s
# end::wait-nifi-rollout[]

sleep 5

case "$1" in
"helm")

echo "Get a single node where a NiFi pod is running"
# tag::get-nifi-node-name[]
nifi_node_name=$(kubectl get endpoints simple-nifi --output=jsonpath='{.subsets[0].addresses[0].nodeName}') && \
echo "NodeName: $nifi_node_name"
# end::get-nifi-node-name[]


if [ "$internal" = true ] ; then
echo "List $nifi_node_name node internal ip"
# tag::get-nifi-node-ip-internal[]
nifi_node_ip=$(kubectl get nodes -o jsonpath="{.items[?(@.metadata.name==\"$nifi_node_name\")].status.addresses[?(@.type==\"InternalIP\")].address}") && \
echo "NodeIp: $nifi_node_ip"
# end::get-nifi-node-ip-internal[]
else
echo "List $nifi_node_name node external ip"
# tag::get-nifi-node-ip-external[]
nifi_node_ip=$(kubectl get nodes -o jsonpath="{.items[?(@.metadata.name==\"$nifi_node_name\")].status.addresses[?(@.type==\"ExternalIP\")].address}") && \
echo "NodeIp: $nifi_node_ip"
# end::get-nifi-node-ip-external[]
fi

echo "Get node port from service"
# tag::get-nifi-service-port[]
nifi_service_port=$(kubectl get service -o jsonpath="{.items[?(@.metadata.name==\"simple-nifi\")].spec.ports[?(@.name==\"https\")].nodePort}") && \
echo "NodePort: $nifi_service_port"
# end::get-nifi-service-port[]

echo "Create NiFi url"
# tag::create-nifi-url[]
nifi_url="https://$nifi_node_ip:$nifi_service_port" && \
echo "NiFi web interface: $nifi_url"
# end::create-nifi-url[]

;;
"stackablectl")

echo "Getting NiFi endpoint with stackablectl ..."
# tag::stackablectl-nifi-url[]
nifi_url=$(stackablectl svc list -o json | jq --raw-output '.nifi[0].endpoints.https')
# end::stackablectl-nifi-url[]
echo "Endpoint: $nifi_url"

;;
*)
echo "Need to provide 'helm' or 'stackablectl' as an argument for which installation method to use!"
exit 1
;;
esac

echo "Starting nifi tests"
chmod +x ./test-nifi.sh
./test-nifi.sh "$nifi_url"
