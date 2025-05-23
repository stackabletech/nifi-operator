# syntax=docker/dockerfile:1.10.0@sha256:865e5dd094beca432e8c0a1d5e1c465db5f998dca4e439981029b3b81fb39ed5
# NOTE: The syntax directive needs to be the first line in a Dockerfile

# =============
# This file is automatically generated from the templates in stackabletech/operator-templating
# DON'T MANUALLY EDIT THIS FILE
# =============

# https://docs.docker.com/build/checks/#fail-build-on-check-violations
# check=error=true

# We want to automatically use the latest. We also don't tag our images with a version.
# hadolint ignore=DL3007
FROM oci.stackable.tech/sdp/ubi9-rust-builder:latest AS builder


# We want to automatically use the latest.
# hadolint ignore=DL3007
FROM registry.access.redhat.com/ubi9/ubi-minimal:latest AS operator

ARG VERSION
ARG RELEASE="1"

# These are chosen at random and are this high on purpose to have very little chance to clash with an existing user or group on the host system
ARG STACKABLE_USER_GID="574654813"
ARG STACKABLE_USER_UID="782252253"

# These labels have mostly been superceded by the OpenContainer spec annotations below but it doesn't hurt to include them
# http://label-schema.org/rc1/
LABEL name="Stackable Operator for Apache NiFi"
LABEL maintainer="info@stackable.tech"
LABEL vendor="Stackable GmbH"
LABEL version="${VERSION}"
LABEL release="${RELEASE}"
LABEL summary="Deploy and manage Apache NiFi clusters."
LABEL description="Deploy and manage Apache NiFi clusters."

# Overwriting/Pinning UBI labels
# https://github.com/projectatomic/ContainerApplicationGenericLabels
LABEL vcs-ref=""
LABEL distribution-scope="public"
LABEL url="https://stackable.tech"
ARG TARGETARCH
LABEL architecture="${TARGETARCH}"
LABEL com.redhat.component=""
# It complains about it being an invalid label but RedHat uses it and we want to override it and it works....
# hadolint ignore=DL3048
LABEL com.redhat.license_terms=""
LABEL io.buildah.version=""
LABEL io.openshift.expose-services=""

# https://github.com/opencontainers/image-spec/blob/036563a4a268d7c08b51a08f05a02a0fe74c7268/annotations.md#annotations
LABEL org.opencontainers.image.authors="info@stackable.tech"
LABEL org.opencontainers.image.url="https://stackable.tech"
LABEL org.opencontainers.image.vendor="Stackable GmbH"
LABEL org.opencontainers.image.licenses="OSL-3.0"
LABEL org.opencontainers.image.documentation="https://docs.stackable.tech/home/stable/nifi/"
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${RELEASE}"
LABEL org.opencontainers.image.title="Stackable Operator for Apache NiFi"
LABEL org.opencontainers.image.description="Deploy and manage Apache NiFi clusters."

# https://docs.openshift.com/container-platform/4.16/openshift_images/create-images.html#defining-image-metadata
# https://github.com/projectatomic/ContainerApplicationGenericLabels/blob/master/vendor/redhat/labels.md
LABEL io.openshift.tags="ubi9,stackable,sdp,nifi"
LABEL io.k8s.description="Deploy and manage Apache NiFi clusters."
LABEL io.k8s.display-name="Stackable Operator for Apache NiFi"

COPY <<EOF /etc/dnf/dnf.conf
[main]
install_weak_deps=0
assumeyes=True
tsflags=nodocs
EOF

RUN <<EOF
# Update image and install kerberos client libraries as well as some other utilities
microdnf update

# **iputils**
# To make debugging easier, includes things like ping
# Added 2024-03: We cannot find any vulnerabilities in the past years
# https://github.com/iputils/iputils
#
# **less**
# To make debugging easier
# Added 2024-03: less has seen three vulnerabilities between 2004 and 2022 which is a risk we're willing to accept for the added convenience
# https://nvd.nist.gov/vuln/search/results?form_type=Advanced&results_type=overview&search_type=all&isCpeNameSearch=false&cpe_vendor=cpe%3A%2F%3Agnu&cpe_product=cpe%3A%2F%3A%3Aless
# cpe:2.3:a:gnu:less:*:*:*:*:*:*:*:*
#
# **nano**
# To make debugging and changing things easier
# Added 2024-03: We checked and it has not seen any vulnerabilities since 2010 (as of 2024-03) we decided to accept it into our base image
# https://nvd.nist.gov/vuln/search/results?form_type=Advanced&results_type=overview&query=cpe%3A2.3%3Aa%3Agnu%3Anano&search_type=all&isCpeNameSearch=false
# cpe:2.3:a:gnu:nano:*:*:*:*:*:*:*:*
#
# **tar**
# To enable kubectl cp
# Added 2024-03: We checked and it has seen eight vulnerabilities since 2001, mostly minor and it's not in executable path so we decided to accept the risk
# https://nvd.nist.gov/vuln/search/results?adv_search=true&isCpeNameSearch=true&query=cpe%3A2.3%3Aa%3Agnu%3Atar%3A-%3A*%3A*%3A*%3A*%3A*%3A*%3A*
# cpe:2.3:a:gnu:tar:-:*:*:*:*:*:*:*
# NOTE (@NickLarsenNZ): Maybe we should consider pinning package versions?
# hadolint ignore=DL3041
microdnf install \
  iputils \
  krb5-libs \
  less \
  libkadm5 \
  nano \
  shadow-utils \
  tar

groupadd --gid ${STACKABLE_USER_GID} --system stackable
# The --no-log-init is required to work around a bug/problem in Go/Docker when very large UIDs are used
# See https://github.com/moby/moby/issues/5419#issuecomment-41478290 for more context
# Making this a system user prevents a mail dir from being created, expiry of passwords etc. but it will warn:
#   useradd warning: stackable's uid 782252253 is greater than SYS_UID_MAX 999
# We can safely ignore this warning, to get rid of the warning we could change /etc/login.defs but that does not seem worth it
# We'll leave the home directory hardcoded to /stackable because I don't want to deal with which chars might be valid and which might not in user name vs. directory
useradd \
  --no-log-init \
  --gid ${STACKABLE_USER_GID} \
  --uid ${STACKABLE_USER_UID} \
  --system \
  --create-home \
  --home-dir /stackable \
   stackable
microdnf remove shadow-utils
microdnf clean all
rm -rf /var/cache/yum

###
### Make shell usage in our containers "nicer"
###
{
  echo "alias ll='ls -alF --color=auto'"
  echo "alias ls='ls --color=auto'"
  echo "alias ..='cd ..'"
  echo "export PS1='\u@\[\e[36m\]\H\[\e[m\] \[\e[32m\]\$(pwd)\[\e[m\] \\$ '"
} >> /stackable/.bashrc

echo -e "if [ -f ~/.bashrc ]; then\n\tsource ~/.bashrc\nfi" >> /stackable/.profile

chown ${STACKABLE_USER_UID}:0 /stackable/.bashrc
chown ${STACKABLE_USER_UID}:0 /stackable/.profile

# All files and folders owned by root to support running as arbitrary users
# This is best practice as all container users will belong to the root group (0)
# This is not very relevant for the operator images but this makes it consistent with `docker-images`
chown -R ${STACKABLE_USER_UID}:0 /stackable
chmod -R g=u /stackable
EOF

COPY <<EOF /README.md
# Stackable Operator for Apache NiFi

* This image contains version ${VERSION} of the operator.
* The operator binary can be found in `/usr/local/bin`
* It is licensed under the OSL-3.0 - the full text can be found in `/licenses/LICENSE`
EOF

COPY LICENSE /licenses/LICENSE

COPY --from=builder --chown=${STACKABLE_USER_UID}:0 /app/* /usr/local/bin/

COPY deploy/config-spec/properties.yaml /etc/stackable/nifi-operator/config-spec/properties.yaml

USER ${STACKABLE_USER_UID}

ENTRYPOINT ["stackable-nifi-operator"]
CMD ["run"]
