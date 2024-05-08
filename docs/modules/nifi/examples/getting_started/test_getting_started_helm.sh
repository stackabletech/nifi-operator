#! /usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
./getting_started.sh helm #ExternalIP commenting this out as the json parsing does not work with e.g. Kind
