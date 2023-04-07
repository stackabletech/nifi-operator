# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- Enabled logging and log aggregation ([#418])
- Deploy default and support custom affinities ([#436], [#451])
- Added the ability to mount extra volumes for files that may be needed for NiFi processors to work ([#434])
- Extend cluster resources for status and cluster operation (paused, stopped) ([#447])
- Cluster status conditions ([#448])

### Changed

- [BREAKING]: Renamed global `config` to `clusterConfig` ([#417])
- [BREAKING]: Moved `zookeeper_configmap_name` to `clusterConfig` ([#417])
- `operator-rs` `0.33.0` -> `0.39.1` ([#418], [#447], [#452])

### Fixed

- Avoid empty log events dated to 1970-01-01 and improve the precision of the
  log event timestamps ([#452]).

[#417]: https://github.com/stackabletech/nifi-operator/pull/417
[#418]: https://github.com/stackabletech/nifi-operator/pull/418
[#434]: https://github.com/stackabletech/nifi-operator/pull/434
[#436]: https://github.com/stackabletech/nifi-operator/pull/436
[#447]: https://github.com/stackabletech/nifi-operator/pull/447
[#448]: https://github.com/stackabletech/nifi-operator/pull/448
[#452]: https://github.com/stackabletech/nifi-operator/pull/452

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

- Support for in-place Nifi cluster upgrades ([#323])
- Added default resource requests (memory and cpu) for Nifi pods ([#353])
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
