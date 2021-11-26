# =============
# This file is automatically generated from the templates in stackabletech/operator-templating
# DON'T MANUALLY EDIT THIS FILE
# =============

.PHONY: docker chart-lint compile-chart

TAG    := $(shell git rev-parse --short HEAD)

VERSION := $(shell cargo metadata --format-version 1 | jq '.packages[] | select(.name=="stackable-nifi-operator") | .version')

docker:
	docker build --force-rm -t "docker.stackable.tech/stackable/nifi-operator:${VERSION}" -f docker/Dockerfile .
	echo "${NEXUS_PASSWORD}" | docker login --username github --password-stdin docker.stackable.tech
	docker push --all-tags docker.stackable.tech/stackable/nifi-operator

## Chart related targets
compile-chart: version crds config 

chart-clean:
	rm -rf deploy/helm/nifi-operator/configs
	rm -rf deploy/helm/nifi-operator/templates/crds.yaml

version:
	yq eval -i '.version = ${VERSION} | .appVersion = ${VERSION}' deploy/helm/nifi-operator/Chart.yaml


config: deploy/helm/nifi-operator/configs

deploy/helm/nifi-operator/configs:
	cp -r deploy/config-spec deploy/helm/nifi-operator/configs

crds: deploy/helm/nifi-operator/crds/crds.yaml

deploy/helm/nifi-operator/crds/crds.yaml:
	mkdir -p deploy/helm/nifi-operator/crds
	cat deploy/crd/*.yaml | yq e '.metadata.annotations["helm.sh/resource-policy"]="keep"' - > ${@}

chart-lint: compile-chart
	docker run -it -v $(shell pwd):/build/helm-charts -w /build/helm-charts quay.io/helmpack/chart-testing:v3.4.0  ct lint --config deploy/helm/chart_testing.yaml
