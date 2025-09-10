#!/bin/bash

TEST_ARGV="$1" docker-compose -f tests/docker/compose/cluster-tls.yml \
  -f tests/docker/runners/compose/cluster-rustls.yml run -u $(id -u ${USER}):$(id -g ${USER}) --rm cluster-rustls-tests