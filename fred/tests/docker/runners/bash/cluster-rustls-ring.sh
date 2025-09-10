#!/bin/bash

declare -a arr=("REDIS_VERSION" "REDIS_USERNAME" "REDIS_PASSWORD", "FRED_TEST_TLS_CREDS")

for env in "${arr[@]}"
do
  if [ -z "$env" ]; then
    echo "$env must be set. Run `source tests/environ` if needed."
    exit 1
  fi
done

FEATURES="enable-rustls-ring transactions i-all network-logs debug-ids"

if [ -z "$FRED_CI_NEXTEST" ]; then
  FRED_CI_TLS=true cargo test --release --lib --tests --features "$FEATURES" -- --test-threads=1 "$@"
else
  FRED_CI_TLS=true cargo nextest run --release --lib --tests --features "$FEATURES" --test-threads=1 "$@"
fi