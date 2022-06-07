#!/bin/bash
git clone -b "$GIT_BRANCH" https://github.com/stackabletech/nifi-operator.git
(cd nifi-operator/ && ./scripts/run_tests.sh --parallel 1)
exit_code=$?
./operator-logs.sh nifi > /target/nifi-operator.log
./operator-logs.sh zookeeper > /target/zookeeper-operator.log
exit $exit_code
