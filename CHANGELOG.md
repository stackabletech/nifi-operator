# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

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
