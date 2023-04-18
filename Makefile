# =============
# This file is automatically generated from the templates in stackabletech/operator-templating
# DO NOT MANUALLY EDIT THIS FILE
# =============

# This script requires https://github.com/mikefarah/yq (not to be confused with https://github.com/kislyuk/yq)
# It is available from Nixpkgs as `yq-go` (`nix shell nixpkgs#yq-go`)
# This script also requires `jq` https://stedolan.github.io/jq/

.PHONY: build publish

TAG    := $(shell git rev-parse --short HEAD)
OPERATOR_NAME := nifi-operator
VERSION := $(shell cargo metadata --format-version 1 | jq -r '.packages[] | select(.name=="stackable-${OPERATOR_NAME}") | .version')

DOCKER_REPO := docker.stackable.tech
ORGANIZATION := stackable
# this will be overwritten by an environmental variable if called from the github action
HELM_REPO := https://repo.stackable.tech/repository/helm-dev
HELM_CHART_ARTIFACT := target/helm/${OPERATOR_NAME}-${VERSION}.tgz

SHELL=/usr/bin/env bash -euo pipefail

render-readme:
	scripts/render_readme.sh

## Docker related targets
docker-build:
	docker build --force-rm --build-arg VERSION=${VERSION} -t "${DOCKER_REPO}/${ORGANIZATION}/${OPERATOR_NAME}:${VERSION}" -f docker/Dockerfile .

docker-publish:
	echo "${NEXUS_PASSWORD}" | docker login --username github --password-stdin "${DOCKER_REPO}"
	docker push --all-tags "${DOCKER_REPO}/${ORGANIZATION}/${OPERATOR_NAME}"

# TODO remove if not used/needed
docker: docker-build docker-publish

print-docker-tag:
	@echo "${DOCKER_REPO}/${ORGANIZATION}/${OPERATOR_NAME}:${VERSION}"

helm-publish:
	curl --fail -u "github:${NEXUS_PASSWORD}" --upload-file "${HELM_CHART_ARTIFACT}" "${HELM_REPO}/"

helm-package:
	mkdir -p target/helm && helm package --destination target/helm deploy/helm/${OPERATOR_NAME}

## Chart related targets
compile-chart: version crds config

chart-clean:
	rm -rf "deploy/helm/${OPERATOR_NAME}/configs"
	rm -rf "deploy/helm/${OPERATOR_NAME}/crds"

version:
	cat "deploy/helm/${OPERATOR_NAME}/Chart.yaml" | yq ".version = \"${VERSION}\" | .appVersion = \"${VERSION}\"" > "deploy/helm/${OPERATOR_NAME}/Chart.yaml.new"
	mv "deploy/helm/${OPERATOR_NAME}/Chart.yaml.new" "deploy/helm/${OPERATOR_NAME}/Chart.yaml"

config:
	if [ -d "deploy/config-spec/" ]; then\
		mkdir -p "deploy/helm/${OPERATOR_NAME}/configs";\
		cp -r deploy/config-spec/* "deploy/helm/${OPERATOR_NAME}/configs";\
	fi

crds:
	mkdir -p deploy/helm/"${OPERATOR_NAME}"/crds
	cargo run --bin stackable-"${OPERATOR_NAME}" -- crd | yq eval '.metadata.annotations["helm.sh/resource-policy"]="keep"' - > "deploy/helm/${OPERATOR_NAME}/crds/crds.yaml"

chart-lint: compile-chart
	docker run -it -v $(shell pwd):/build/helm-charts -w /build/helm-charts quay.io/helmpack/chart-testing:v3.5.0  ct lint --config deploy/helm/ct.yaml

clean: chart-clean
	cargo clean
	docker rmi --force "${DOCKER_REPO}/${ORGANIZATION}/${OPERATOR_NAME}:${VERSION}"

regenerate-charts: chart-clean compile-chart

build: regenerate-charts helm-package docker-build

publish: build docker-publish helm-publish

run-dev:
	kubectl apply -f deploy/stackable-operators-ns.yaml
	nix run -f. tilt -- up --port 5437 --namespace stackable-operators
