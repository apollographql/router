#!/bin/bash

TEST_ARGV="$1" docker-compose -f tests/docker/compose/valkey-centralized.yml \
  -f tests/docker/compose/valkey-cluster.yml \
  -f tests/docker/runners/compose/valkey-all-features.yml \
  run -u $(id -u ${USER}):$(id -g ${USER}) --rm valkey-all-features-tests