# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Added versioning code from operator-rs for up and downgrades ([#81]).
- Added `ProductVersion` to status ([#81]).
- Added `Condition` to status ([#81]).

[#81]: https://github.com/stackabletech/nifi-operator/pull/81

## [0.2.0] - 2021-09-14

### Changed
- **Breaking:** Repository structure was changed and the -server crate renamed to -binary. As part of this change the -server suffix was removed from both the package name for os packages and the name of the executable ([#72]).

[#72]: https://github.com/stackabletech/nifi-operator/pull/72

## [0.1.0] - 2021.09.07

### Added

- Initial release
