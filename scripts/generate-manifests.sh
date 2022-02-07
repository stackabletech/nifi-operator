#!/usr/bin/env bash
# This script reads a Helm chart from deploy/helm/nifi-operator and
# generates manifest files into deploy/manifestss
set -e

tmp=$(mktemp -d ./manifests-XXXXX)

helm template --output-dir "$tmp" \
              --include-crds \
              --name-template nifi-operator \
              deploy/helm/nifi-operator

while IFS= read -r -d '' file
do
    yq eval -i 'del(.. | select(has("app.kubernetes.io/managed-by")) | ."app.kubernetes.io/managed-by")' "$file"
    yq eval -i 'del(.. | select(has("helm.sh/chart")) | ."helm.sh/chart")' "$file"
    sed -i '/# Source: .*/d' "$file"
done <   <(find "$tmp" -type f)

cp -r "$tmp"/nifi-operator/*/* deploy/manifests/

rm -rf "$tmp"
