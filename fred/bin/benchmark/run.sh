#!/bin/bash

[[ -z "${REDIS_RS_BB8}" ]] && FEATURES="assert-expected" || FEATURES="assert-expected redis-rs"

if [[ ! -z "${REDIS_RS_MANAGER}" ]]; then
  FEATURES="assert-expected redis-rs redis-manager"
fi

# echo 0 | sudo tee /proc/sys/kernel/kptr_restrict
# echo "-1" | sudo tee /proc/sys/kernel/perf_event_paranoid
echo $FEATURES

docker-compose -f ../../tests/docker/compose/cluster.yml \
  -f ../../tests/docker/compose/centralized.yml \
  -f ../../tests/docker/compose/unix-socket.yml \
  -f ./docker-compose.yml \
  run -u $(id -u ${USER}):$(id -g ${USER}) --rm fred-benchmark cargo run --release --features "$FEATURES" -- "${@:1}"