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

echo "Install NiFiCluster from nifi.yaml"
# tag::install-nifi[]
kubectl apply -f nifi.yaml
# end::install-nifi[]

sleep 5

echo "Awaiting NiFi rollout finish"
# tag::watch-nifi-rollout[]
kubectl wait -l statefulset.kubernetes.io/pod-name=simple-nifi-node-default-0 --for=condition=ready pod --timeout=-5s
kubectl wait -l statefulset.kubernetes.io/pod-name=simple-nifi-node-default-1 --for=condition=ready pod --timeout=-5s
# end::watch-nifi-rollout[]

sleep 5

# get user password
echo "Getting NiFi credentials"
nifi_username=$(kubectl get secret nifi-admin-credentials-simple -o jsonpath='{.data.username}' | base64 --decode)
nifi_password=$(kubectl get secret nifi-admin-credentials-simple -o jsonpath='{.data.password}' | base64 --decode)

# get nifi host
echo "Getting NiFi host url"
nifi_host=( $(kubectl get svc simple-nifi -o json | jq -r --argfile endpoints <(kubectl get endpoints simple-nifi -o json) --argfile nodes <(kubectl get nodes -o json) '($nodes.items[] | select(.metadata.name == $endpoints.subsets[].addresses[].nodeName) | .status.addresses | map(select(.type == "ExternalIP" or .type == "InternalIP")) | min_by(.type) | .address | tostring) + ":" + (.spec.ports[] | select(.name == "https") | .nodePort | tostring)') )

# get nifi token
echo "Getting NiFi JWT token"
nifi_jwt_token=$(curl -s -X POST --insecure --header 'content-type: application/x-www-form-urlencoded' -d "username=$nifi_username&password=$nifi_password" "https://$nifi_host/nifi-api/access/token") || true

x=1
retry_interval_seconds=10
expected_nodes=2

while [ $x -le 20 ]
do
  # get amount of nodes from cluster status
  echo -n "Checking if NiFi cluster is ready ... "
  http_code=$(curl --insecure -s -o /dev/null -w "%{http_code}" --header "Authorization: $nifi_jwt_token" https://"$nifi_host"/nifi-api/controller/cluster)

  if [ "$http_code" -ne "200" ]; then
    echo "not ready yet!"
  else
    echo "yes"
    echo -n "Checking if NiFi cluster has $expected_nodes nodes ... "
    cluster_status=$(curl -s --insecure --header "Authorization: $nifi_jwt_token" https://"$nifi_host"/nifi-api/controller/cluster)
    nodes=$( echo "$cluster_status" | jq .cluster.nodes | jq 'length')

    if [ "$?" -eq 0 ] && [ "$nodes" -eq "$expected_nodes" ]; then
      echo "yes"
      echo "NiFi cluster started up successfully!"
      exit 0
    else
      echo "no"
    fi
  fi

  echo "Retrying in $retry_interval_seconds seconds..."
  x=$(( $x + 1 ))
  sleep "$retry_interval_seconds"
done

echo "Could not verify that NiFi cluster works correctly. Setup failed!"
exit 1
