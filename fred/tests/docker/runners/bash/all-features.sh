#!/bin/bash

declare -a arr=("REDIS_VERSION" "REDIS_USERNAME" "REDIS_PASSWORD" "REDIS_SENTINEL_PASSWORD")

for env in "${arr[@]}"
do
  if [ -z "$env" ]; then
    echo "$env must be set. Run `source tests/environ` if needed."
    exit 1
  fi
done

# can't use --all-features here since that enables the TLS features and redis-json tests, which each require a
# different server configuration. the `cluster-tls.sh` and `cluster-rustls.sh` scripts can be used to test
# those features individually.
FEATURES="network-logs custom-reconnect-errors serde-json blocking-encoding dynamic-pool
          full-tracing monitor metrics sentinel-client subscriber-client dns debug-ids
          replicas sha-1 transactions i-all credential-provider tcp-user-timeouts"

if [ -z "$FRED_CI_NEXTEST" ]; then
  cargo test --release --lib --tests --features "$FEATURES" -- --test-threads=1 "$@"
else
  cargo nextest run --release --lib --tests --features "$FEATURES" --test-threads=1 "$@"
fi