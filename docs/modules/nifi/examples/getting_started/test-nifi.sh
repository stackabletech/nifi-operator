#!/usr/bin/env bash
set -euo pipefail

nifi_host=$1

# get user password
echo "Getting NiFi credentials"
nifi_username=$(kubectl get secret nifi-admin-credentials-simple -o jsonpath='{.data.username}' | base64 --decode)
nifi_password=$(kubectl get secret nifi-admin-credentials-simple -o jsonpath='{.data.password}' | base64 --decode)

# check if host is reachable
return_code=$(curl --insecure -s -o /dev/null -w "%{http_code}" "$nifi_host")

if [ "$return_code" -ne "200" ]; then
  echo "can't reach NiFi. return code: $return_code"
  exit 1
fi

# get nifi token
echo "Getting NiFi JWT token"
nifi_jwt_token=$(curl -s -X POST --insecure --header 'content-type: application/x-www-form-urlencoded' -d "username=$nifi_username&password=$nifi_password" "$nifi_host/nifi-api/access/token")

x=1
retry_interval_seconds=10
expected_nodes=2

while [ $x -le 15 ]
do
  echo -n "Checking if NiFi cluster is ready ... "
  http_code=$(curl --insecure -s -o /dev/null -w "%{http_code}" --header "Authorization: Bearer $nifi_jwt_token" "$nifi_host/nifi-api/controller/cluster")

  if [ "$http_code" -ne "200" ]; then
    echo "not ready yet!"
  else
    echo "yes"
    echo -n "Checking if NiFi cluster has $expected_nodes nodes ... "
    cluster_status=$(curl -s --insecure --header "Authorization: Bearer $nifi_jwt_token" "$nifi_host/nifi-api/controller/cluster")
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
  x=$(( x + 1 ))
  sleep "$retry_interval_seconds"
done

echo "Could not verify that NiFi cluster works correctly. Setup failed!"
exit 1
