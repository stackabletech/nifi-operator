# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- Support OpenID Connect authentication ([#660]).
- Allow configuring proxy host behavior ([#668]).
- Support disabling the `create-reporting-task` Job ([#690]).
- Support podOverrides on the `create-reporting-task` Job using the field `spec.clusterConfig.createReportingTaskJob.podOverrides` ([#690]).
- The operator can now run on Kubernetes clusters using a non-default cluster domain.
  Use the env var `KUBERNETES_CLUSTER_DOMAIN` or the operator Helm chart property `kubernetesClusterDomain` to set a non-default cluster domain ([#694]).

### Changed

- Reduce CRD size from `637KB` to `105KB` by accepting arbitrary YAML input instead of the underlying schema for the following fields ([#664]):
  - `podOverrides`
  - `affinity`
  - `extraVolumes`
- Increase `log` Volume size from 33 MiB to 500 MiB ([#671]).
- Replaced experimental NiFi `2.0.0-M4` with `2.0.0` ([#xxx]).

### Fixed

- Switch from `flow.xml.gz` to `flow.json.gz` to allow seamless upgrades to version 2.0 ([#675]).
- Failing to parse one `NifiCluster`/`AuthenticationClass` should no longer cause the whole operator to stop functioning ([#662]).
- NiFi will now use the JDK trust store when an OIDC provider uses WebPKI as CA ([#686], [#698]).

### Removed

- Removed support for NiFi versions 1.21.0 and 1.25.0 ([#665]).
- test: Remove ZooKeeper 3.8.4 ([#672]).

[#660]: https://github.com/stackabletech/nifi-operator/pull/660
[#662]: https://github.com/stackabletech/nifi-operator/pull/662
[#664]: https://github.com/stackabletech/nifi-operator/pull/664
[#665]: https://github.com/stackabletech/nifi-operator/pull/665
[#668]: https://github.com/stackabletech/nifi-operator/pull/668
[#671]: https://github.com/stackabletech/nifi-operator/pull/671
[#672]: https://github.com/stackabletech/nifi-operator/pull/672
[#675]: https://github.com/stackabletech/nifi-operator/pull/675
[#686]: https://github.com/stackabletech/nifi-operator/pull/686
[#690]: https://github.com/stackabletech/nifi-operator/pull/690
[#694]: https://github.com/stackabletech/nifi-operator/pull/694
[#698]: https://github.com/stackabletech/nifi-operator/pull/698
[#xxx]: https://github.com/stackabletech/nifi-operator/pull/xxx

## [24.7.0] - 2024-07-24

### Added

- Support specifying the SecretClass that is used to obtain TLS certificates ([#622]).
- Support for NiFi `1.27.0` and `2.0.0-M4` ([#639]).

### Changed

- Bump `stackable-operator` from `0.64.0` to `0.70.0` ([#641]).
- Bump `product-config` from `0.6.0` to `0.7.0` ([#641]).
- Bump other dependencies ([#642]).
- Make it easy to test custom NiFi images ([#616]).

### Fixed

- Use [config-utils](https://github.com/stackabletech/config-utils/) for text-replacement of variables in configs.
  This fixes escaping problems, especially when you have special characters in your password ([#627]).
- Processing of corrupted log events fixed; If errors occur, the error
  messages are added to the log event ([#628]).

### Removed

- Removed support for `1.23.2` ([#639]).

[#616]: https://github.com/stackabletech/nifi-operator/pull/616
[#622]: https://github.com/stackabletech/nifi-operator/pull/622
[#627]: https://github.com/stackabletech/nifi-operator/pull/627
[#628]: https://github.com/stackabletech/nifi-operator/pull/628
[#639]: https://github.com/stackabletech/nifi-operator/pull/639
[#641]: https://github.com/stackabletech/nifi-operator/pull/641
[#642]: https://github.com/stackabletech/nifi-operator/pull/642

## [24.3.0] - 2024-03-20

### Added

- Various documentation of the CRD ([#537]).
- Document support for Apache Iceberg extensions ([#556]).
- Helm: support labels in values.yaml ([#560]).
- Support for NiFi `1.25.0` ([#571]).

### Changed

- A service for a single NiFi node is created for the reporting task to avoid JWT issues ([#571]).

[#537]: https://github.com/stackabletech/nifi-operator/pull/537
[#556]: https://github.com/stackabletech/nifi-operator/pull/556
[#560]: https://github.com/stackabletech/nifi-operator/pull/560
[#571]: https://github.com/stackabletech/nifi-operator/pull/571

## [23.11.0] - 2023-11-24

### Added

- Default stackableVersion to operator version. It is recommended to remove `spec.image.stackableVersion` from your custom resources ([#493]).
- Configuration overrides for the JVM security properties, such as DNS caching ([#497]).
- Support PodDisruptionBudgets ([#509]).
- Support for 1.23.2 ([#513]).
- Support graceful shutdown ([#528]).

### Changed

- `vector` `0.26.0` -> `0.33.0` ([#494], [#513]).
- `operator-rs` `0.44.0` -> `0.55.0` ([#493], [#498], [#509], [#513]).
- [BREAKING] Consolidated authentication config to a list of AuthenticationClasses ([#498]).
- Let secret-operator handle certificate conversion ([#505]).

### Removed

- [BREAKING] Removed crd support for nifi.security.allow.anonymous.authentication that was never actually used ([#498]).
- [BREAKING] Removed crd support for the auto generation of admin credentials (obsolete since the user now always has to provide an AuthenticationClass) ([#498]).
- Support for 1.15.x, 1.16.x, 1.18.x, 1.20.x ([#513]).

[#493]: https://github.com/stackabletech/nifi-operator/pull/493
[#494]: https://github.com/stackabletech/nifi-operator/pull/494
[#497]: https://github.com/stackabletech/nifi-operator/pull/497
[#498]: https://github.com/stackabletech/nifi-operator/pull/498
[#505]: https://github.com/stackabletech/nifi-operator/pull/505
[#509]: https://github.com/stackabletech/nifi-operator/pull/509
[#513]: https://github.com/stackabletech/nifi-operator/pull/513
[#528]: https://github.com/stackabletech/nifi-operator/pull/528

## [23.7.0] - 2023-07-14

### Added

- Added support for NiFi versions 1.20.0 and 1.21.0 ([#464]).
- Generate OLM bundle for Release 23.4.0 ([#467]).
- Missing CRD defaults for `status.conditions` field ([#471]).
- Set explicit resources on all containers ([#476]).
- Support podOverrides ([#483]).

### Changed

- `operator-rs` `0.40.2` -> `0.44.0` ([#461], [#486]).
- Use 0.0.0-dev product images for testing ([#463])
- Use testing-tools 0.2.0 ([#463])
- Added kuttl test suites ([#480])

### Fixed

- Use ou with spaces in LDAP tests ([#466]).
- Reporting task now escapes user and password input in case of whitespaces ([#466]).
- Increase the size limit of the log volume ([#486]).

[#461]: https://github.com/stackabletech/nifi-operator/pull/461
[#463]: https://github.com/stackabletech/nifi-operator/pull/463
[#464]: https://github.com/stackabletech/nifi-operator/pull/464
[#466]: https://github.com/stackabletech/nifi-operator/pull/466
[#467]: https://github.com/stackabletech/nifi-operator/pull/467
[#471]: https://github.com/stackabletech/nifi-operator/pull/471
[#476]: https://github.com/stackabletech/nifi-operator/pull/476
[#480]: https://github.com/stackabletech/nifi-operator/pull/480
[#483]: https://github.com/stackabletech/nifi-operator/pull/483
[#486]: https://github.com/stackabletech/nifi-operator/pull/486

## [23.4.0] - 2023-04-17

### Added

- Enabled logging and log aggregation ([#418])
- Deploy default and support custom affinities ([#436], [#451])
- Added the ability to mount extra volumes for files that may be needed for NiFi processors to work ([#434])
- Openshift compatibility ([#446]).
- Extend cluster resources for status and cluster operation (paused, stopped) ([#447])
- Cluster status conditions ([#448])

### Changed

- [BREAKING]: Renamed global `config` to `clusterConfig` ([#417])
- [BREAKING]: Moved `zookeeper_configmap_name` to `clusterConfig` ([#417])
- `operator-rs` `0.33.0` -> `0.40.2` ([#418], [#447], [#452])
- [BREAKING] Support specifying Service type.
  This enables us to later switch non-breaking to using `ListenerClasses` for the exposure of Services.
  This change is breaking, because - for security reasons - we default to the `cluster-internal` `ListenerClass`.
  If you need your cluster to be accessible from outside of Kubernetes you need to set `clusterConfig.listenerClass`
  to `external-unstable` ([#449]).

### Fixed

- Avoid empty log events dated to 1970-01-01 and improve the precision of the
  log event timestamps ([#452]).
- Fix `create-reporting-task` to support multiple rolegroups ([#453])
- Fix proxy hosts list missing an entry for the load-balanced Service ([#453])
- Remove hardcoded `kubernetes.io/os=linux` selector when determining list of valid proxy nodes ([#453])

[#417]: https://github.com/stackabletech/nifi-operator/pull/417
[#418]: https://github.com/stackabletech/nifi-operator/pull/418
[#434]: https://github.com/stackabletech/nifi-operator/pull/434
[#436]: https://github.com/stackabletech/nifi-operator/pull/436
[#446]: https://github.com/stackabletech/nifi-operator/pull/446
[#447]: https://github.com/stackabletech/nifi-operator/pull/447
[#448]: https://github.com/stackabletech/nifi-operator/pull/448
[#449]: https://github.com/stackabletech/nifi-operator/pull/449
[#451]: https://github.com/stackabletech/nifi-operator/pull/451
[#452]: https://github.com/stackabletech/nifi-operator/pull/452
[#453]: https://github.com/stackabletech/nifi-operator/pull/453

## [23.1.0] - 2023-01-23

### Changed

- Updated operator-rs to 0.31.0 ([#382], [#401], [#408])
- Do not run init container as root anymore and avoid chmod and chown ([#390])
- [BREAKING] Use Product image selection instead of version. `spec.version` has been replaced by `spec.image` ([#394])
- [BREAKING]: Removed tools image (reporting task job and init container) and replaced with NiFi product image. This means the latest stackable version has to be used in the product image selection ([#397])
- Fixed the RoleGroup `selector`. It was not used before. ([#401])
- Refactoring of authentication handling ([#408])

[#382]: https://github.com/stackabletech/nifi-operator/pull/382
[#390]: https://github.com/stackabletech/nifi-operator/pull/390
[#394]: https://github.com/stackabletech/nifi-operator/pull/394
[#397]: https://github.com/stackabletech/nifi-operator/pull/397
[#401]: https://github.com/stackabletech/nifi-operator/pull/401
[#408]: https://github.com/stackabletech/nifi-operator/pull/408

## [0.8.1] - 2022-11-10

### Changed

- Fixed a regression that made PVC configs mandatory in some cases ([#375])
- Updated stackable image versions ([#376])

[#375]: https://github.com/stackabletech/nifi-operator/pull/375
[#376]: https://github.com/stackabletech/nifi-operator/pull/376

## [0.8.0] - 2022-11-08

### Added

- Support for in-place NiFi cluster upgrades ([#323])
- Added default resource requests (memory and cpu) for NiFi pods ([#353])
- Added support for NiFi version 1.18.0 ([#360])

### Changed

- Updated operator-rs to 0.26.1 ([#371])
- NiFi repository sizes are now adjusted based on declared PVC sizes ([#371])

[#323]: https://github.com/stackabletech/nifi-operator/pull/323
[#353]: https://github.com/stackabletech/nifi-operator/pull/353
[#360]: https://github.com/stackabletech/nifi-operator/pull/360
[#371]: https://github.com/stackabletech/nifi-operator/pull/371

## [0.7.0] - 2022-09-06

### Added

- Add support for LDAP authentication ([#303], [#318])

### Changed

- Include chart name when installing with a custom release name ([#300], [#301]).
- Orphaned resources are deleted ([#319])
- Updated operator-rs to 0.25.0 ([#319], [#328])
- Operator will not error out any more if admin credential need to be generated but `auto_generate` is not set.
  Instead the pods are written but will stay in initializing state until the necessary secrets have been
  created. ([#319])

[#300]: https://github.com/stackabletech/nifi-operator/pull/300
[#301]: https://github.com/stackabletech/nifi-operator/pull/301
[#303]: https://github.com/stackabletech/nifi-operator/pull/303
[#318]: https://github.com/stackabletech/nifi-operator/pull/318
[#319]: https://github.com/stackabletech/nifi-operator/pull/319
[#328]: https://github.com/stackabletech/nifi-operator/pull/328

## [0.6.0] - 2022-06-30

### Added

- Reconciliation errors are now reported as Kubernetes events ([#218]).
- Use cli argument `watch-namespace` / env var `WATCH_NAMESPACE` to specify
  a single namespace to watch ([#223]).
- Enable prometheus metrics via a `Job`. This is done via a python script that creates a ReportingTask via the NiFi REST API in the `tools` docker image ([#230]).
- Monitoring scraping label prometheus.io/scrape: true ([#230]).

### Changed

- `operator-rs` `0.10.0` -> `0.15.0` ([#218], [#223], [#230]).
- [BREAKING] Specifying the product version has been changed to adhere to [ADR018](https://docs.stackable.tech/home/contributor/adr/ADR018-product_image_versioning.html) instead of just specifying the product version you will now have to add the Stackable image version as well, so `version: 3.5.8` becomes (for example) `version: 3.5.8-stackable0.1.0` ([#270])
- [BREAKING] CRD overhaul: Moved `authenticationConfig` to top level `config.authentication`. `SingleUser` now proper camelCase `singleUser`. `adminCredentialsSecret` now takes a String instead of `SecretReference` ([#277]).
- [BREAKING] CRD overhaul: Moved `sensitivePropertiesConfig` to top level `config.sensitiveProperties` ([#277]).

### Removed

- The `monitoring.rs` module which is obsolete ([#230]).

[#218]: https://github.com/stackabletech/nifi-operator/pull/218
[#223]: https://github.com/stackabletech/nifi-operator/pull/223
[#230]: https://github.com/stackabletech/nifi-operator/pull/230
[#270]: https://github.com/stackabletech/nifi-operator/pull/270
[#277]: https://github.com/stackabletech/nifi-operator/pull/277

## [0.5.0] - 2022-02-14

### Changed

- The ZooKeeper discovery now references config map name of ZNode ([#207]).
- `operator-rs` `0.9.0` → `0.10.0` ([#207]).

[#207]: https://github.com/stackabletech/nifi-operator/pull/207

## [0.4.0] - 2021-12-06

### Added

- Support for 1.15.0 ([#125])
- Sensitive property key is setable via a secret ([#125])

### Changed

- Removed support for 1.13.2 ([#125])
- Added/removed some default config settings that changed from 1.13 to 1.15 ([#125])
- `operator-rs` `0.3.0` → `0.4.0` ([#101]).
- `stackable-zookeeper-crd`: `0.4.1` → `0.5.0` ([#101]).
- Adapted pod image and container command to docker image ([#101]).
- Adapted documentation to represent new workflow with docker images ([#101]).

[#101]: https://github.com/stackabletech/nifi-operator/pull/101
[#125]: https://github.com/stackabletech/nifi-operator/pull/125

## [0.3.0] - 2021-10-27

### Added

- Added versioning code from operator-rs for up and downgrades ([#81]).
- Added `ProductVersion` to status ([#81]).
- Added `Condition` to status ([#81]).
- Use sticky scheduler ([#87])

### Changed

- `stackable-zookeeper-crd`: `0.3.0` → `0.4.1` ([#92]).
- `operator-rs`: `0.3.0` ([#92]).
- `kube-rs`: `0.58` → `0.60` ([#83]).
- `k8s-openapi` `0.12` → `0.13` and features: `v1_21` → `v1_22` ([#83]).
- `operator-rs` `0.2.1` → `0.2.2` ([#83]).

### Fixed

- Fixed a bug where `wait_until_crds_present` only reacted to the main CRD, not the commands ([#92]).

[#92]: https://github.com/stackabletech/nifi-operator/pull/92
[#83]: https://github.com/stackabletech/nifi-operator/pull/83
[#81]: https://github.com/stackabletech/nifi-operator/pull/81
[#87]: https://github.com/stackabletech/nifi-operator/pull/87

## [0.2.0] - 2021-09-14

### Changed

- **Breaking:** Repository structure was changed and the -server crate renamed to -binary. As part of this change the -server suffix was removed from both the package name for os packages and the name of the executable ([#72]).

[#72]: https://github.com/stackabletech/nifi-operator/pull/72

## [0.1.0] - 2021.09.07

### Added

- Initial release
