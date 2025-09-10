#!/bin/bash

TEST_ARGV="$1" docker-compose -f tests/docker/compose/cluster-tls.yml \
  -f tests/docker/runners/compose/cluster-rustls-ring.yml run -u $(id -u ${USER}):$(id -g ${USER}) --rm cluster-rustls-ring-tests