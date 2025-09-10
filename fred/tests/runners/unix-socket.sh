#!/bin/bash

TEST_ARGV="$1" docker-compose -f tests/docker/compose/unix-socket.yml \
  -f tests/docker/runners/compose/unix-socket.yml run \
  -u $(id -u ${USER}):$(id -g ${USER}) --rm unix-socket-tests