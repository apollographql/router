#!/bin/bash

TEST_ARGV="$1" docker-compose -f tests/docker/compose/redis-stack.yml \
  -f tests/docker/runners/compose/redis-stack.yml run \
  -u $(id -u ${USER}):$(id -g ${USER}) --rm redis-stack-tests