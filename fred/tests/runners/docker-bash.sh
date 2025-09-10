#!/bin/bash

# boot all the redis servers and start a bash shell on a new container
# This uses redis-stack instead of `-f tests/docker/compose/centralized.yml` since
# they compete for a port and it's not easy to change the redis-stack port.
docker-compose -f tests/docker/compose/cluster-tls.yml \
  -f tests/docker/compose/cluster.yml \
  -f tests/docker/compose/sentinel.yml \
  -f tests/docker/compose/redis-stack.yml \
  -f tests/docker/compose/centralized.yml \
  -f tests/docker/compose/valkey-cluster.yml \
  -f tests/docker/compose/base.yml run --rm debug

