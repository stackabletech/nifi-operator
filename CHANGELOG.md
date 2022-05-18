# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- Reconciliation errors are now reported as Kubernetes events ([#218]).
- Use cli argument `watch-namespace` / env var `WATCH_NAMESPACE` to specify
  a single namespace to watch ([#223]).
- Enable prometheus metrics via a `Job`. This is done via a python script that creates a ReportingTask via the NiFi REST API in the `tools` docker image ([#230]).
- Monitoring scraping label prometheus.io/scrape: true ([#230]).

### Changed

- `operator-rs` `0.10.0` -> `0.15.0` ([#218], [#223], [#230]).
- [BREAKING] Specifying the product version has been changed to adhere to [ADR018](https://docs.stackable.tech/home/contributor/adr/ADR018-product_image_versioning.html) instead of just specifying the product version you will now have to add the Stackable image version as well, so `version: 3.5.8` becomes (for example) `version: 3.5.8-stackable0.1.0` ([#270])

### Removed

- The `monitoring.rs` module which is obsolete ([#230]).

[#218]: https://github.com/stackabletech/nifi-operator/pull/218
[#223]: https://github.com/stackabletech/nifi-operator/pull/223
[#230]: https://github.com/stackabletech/nifi-operator/pull/230
[#270]: https://github.com/stackabletech/nifi-operator/pull/270

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
