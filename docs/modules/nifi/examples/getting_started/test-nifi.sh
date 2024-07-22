#!/usr/bin/env bash
set -euo pipefail

nifi_host=$1

# get user password (admin is fixed as username)
echo "Getting NiFi credentials"
nifi_username="admin"
nifi_password=$(kubectl get secret simple-admin-credentials -o jsonpath='{.data.admin}' | base64 --decode)

# check if host is reachable
echo "Checking if NiFi is reachable at $nifi_host"
return_code=$(curl --insecure -v -o /dev/null -w "%{http_code}" "$nifi_host")

if [ "$return_code" -ne "200" ]; then
  echo "can't reach NiFi. return code: $return_code"
  exit 1
fi

# get nifi token
echo "Getting NiFi JWT token"
nifi_jwt_token=$(curl -s -X POST --insecure --header 'content-type: application/x-www-form-urlencoded' -d "username=$nifi_username&password=$nifi_password" "$nifi_host/nifi-api/access/token")

# tunables
retry_interval_seconds=10
retry_attempts=15
expected_nodes=1

attempt=1
while [ $attempt -le $retry_attempts ]
do
  echo -n "Checking if NiFi cluster is ready ($attempt/$retry_attempts) ... "
  http_code=$(curl --insecure -s -o /dev/null -w "%{http_code}" --header "Authorization: Bearer $nifi_jwt_token" "$nifi_host/nifi-api/controller/cluster")

  if [ "$http_code" -ne "200" ]; then
    echo "not ready yet!"
  else
    echo "yes"
    echo -n "Checking if NiFi cluster has $expected_nodes nodes ... "
    cluster_status=$(curl -s --insecure --header "Authorization: Bearer $nifi_jwt_token" "$nifi_host/nifi-api/controller/cluster")
    nodes=$( echo "$cluster_status" | jq .cluster.nodes | jq 'length')

    # shellcheck disable=SC2181 # wont't fix this now, but ideally we should enable bash strict mode so we can avoid success checks.
    if [ "$?" -eq 0 ] && [ "$nodes" -eq "$expected_nodes" ]; then
      echo "yes"
      echo "NiFi cluster started up successfully!"
      exit 0
    else
      echo "no"
    fi
  fi

  echo "Retrying in $retry_interval_seconds seconds..."
  attempt=$(( attempt + 1 ))
  sleep "$retry_interval_seconds"
done

echo "Could not verify that NiFi cluster works correctly. Setup failed!"
exit 1
