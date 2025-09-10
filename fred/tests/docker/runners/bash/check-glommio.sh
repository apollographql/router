#!/bin/bash

FEATURES="network-logs custom-reconnect-errors serde-json blocking-encoding credential-provider
          full-tracing monitor metrics sentinel-client subscriber-client dns debug-ids mocks
          replicas sha-1 transactions i-all glommio i-redis-stack enable-rustls enable-native-tls"

cargo clippy --features "$FEATURES" -- "$@"