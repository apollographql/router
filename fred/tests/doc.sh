#!/bin/bash


FEATURES="network-logs custom-reconnect-errors serde-json blocking-encoding credential-provider dynamic-pool
          full-tracing monitor metrics sentinel-client subscriber-client dns debug-ids sentinel-auth
          replicas sha-1 transactions i-all unix-sockets i-redis-stack enable-rustls enable-native-tls tcp-user-timeouts"

cargo +nightly rustdoc --features "$FEATURES" "$@" -- --cfg docsrs