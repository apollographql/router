#!/bin/bash

FEATURES="network-logs custom-reconnect-errors serde-json blocking-encoding credential-provider mocks
          full-tracing monitor metrics sentinel-client subscriber-client dns debug-ids sentinel-auth
          replicas sha-1 transactions i-all glommio i-redis-stack enable-rustls enable-native-tls"

RUSTDOCFLAGS="" cargo +nightly rustdoc --features "$FEATURES" "$@" -- --cfg docsrs