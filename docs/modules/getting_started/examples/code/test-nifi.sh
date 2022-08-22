#!/usr/bin/env bash
set -euo pipefail

echo "Awaiting NiFi rollout finish"
# tag::wait-nifi-rollout[]
kubectl wait -l statefulset.kubernetes.io/pod-name=simple-nifi-node-default-0 --for=condition=ready pod --timeout=-5s
kubectl wait -l statefulset.kubernetes.io/pod-name=simple-nifi-node-default-1 --for=condition=ready pod --timeout=-5s
# end::wait-nifi-rollout[]

sleep 5

# get user password
echo "Getting NiFi credentials"
# tag::get-nifi-credentials[]
nifi_username=$(kubectl get secret nifi-admin-credentials-simple -o jsonpath='{.data.username}' | base64 --decode)
nifi_password=$(kubectl get secret nifi-admin-credentials-simple -o jsonpath='{.data.password}' | base64 --decode)
# end::get-nifi-credentials[]

# get nifi host
echo "Getting NiFi host url"
# tag::get-nifi-host[]
nifi_host=( $(kubectl get svc simple-nifi -o json | jq -r --argfile endpoints <(kubectl get endpoints simple-nifi -o json) --argfile nodes <(kubectl get nodes -o json) '($nodes.items[] | select(.metadata.name == $endpoints.subsets[].addresses[].nodeName) | .status.addresses | map(select(.type == "ExternalIP" or .type == "InternalIP")) | min_by(.type) | .address | tostring) + ":" + (.spec.ports[] | select(.name == "https") | .nodePort | tostring)') )
# end::get-nifi-host[]

# get nifi token
echo "Getting NiFi JWT token"
# tag::get-nifi-token[]
nifi_jwt_token=$(curl -s -X POST --insecure --header 'content-type: application/x-www-form-urlencoded' -d "username=$nifi_username&password=$nifi_password" "https://$nifi_host/nifi-api/access/token")
# end::get-nifi-token[]

# tag::nifi-test-loop[]
x=1
retry_interval_seconds=10
expected_nodes=2

while [ $x -le 15 ]
do
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
# end::nifi-test-loop[]
