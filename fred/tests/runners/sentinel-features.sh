#!/bin/bash

TEST_ARGV="$1" docker-compose -f tests/docker/compose/sentinel.yml \
  -f tests/docker/runners/compose/sentinel-features.yml \
  run -u $(id -u ${USER}):$(id -g ${USER}) --rm sentinel-tests