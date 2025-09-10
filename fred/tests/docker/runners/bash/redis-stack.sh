#!/bin/bash

declare -a arr=("REDIS_VERSION" "REDIS_PASSWORD" "FRED_REDIS_STACK_HOST" "FRED_REDIS_STACK_PORT")

for env in "${arr[@]}"
do
  if [ -z "$env" ]; then
    echo "$env must be set. Run `source tests/environ` if needed."
    exit 1
  fi
done

FEATURES="network-logs serde-json debug-ids i-redis-stack i-all i-hexpire"

if [ -z "$FRED_CI_NEXTEST" ]; then
  cargo test --release --lib --tests --features "$FEATURES" -- --test-threads=1 "$@"
else
  cargo nextest run --release --lib --tests --features "$FEATURES" --test-threads=1 "$@"
fi