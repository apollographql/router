#!/bin/bash

rm -rf .doc
mkdir -p .doc .doc/tokio .doc/glommio

FEATURES="network-logs custom-reconnect-errors serde-json blocking-encoding unix-sockets mocks
          full-tracing monitor metrics sentinel-client subscriber-client dns debug-ids sentinel-auth
          replicas sha-1 transactions i-all i-redis-stack enable-rustls enable-native-tls credential-provider"

cargo +nightly rustdoc --features "$FEATURES" "$@" -- --cfg docsrs
mv target/doc/* .doc/tokio/

FEATURES="network-logs custom-reconnect-errors serde-json blocking-encoding mocks sentinel-auth
          full-tracing monitor metrics sentinel-client subscriber-client dns debug-ids credential-provider
          replicas sha-1 transactions i-all glommio i-redis-stack enable-rustls enable-native-tls"

cargo +nightly rustdoc --features "$FEATURES" "$@" -- --cfg docsrs
mv target/doc/* .doc/glommio/

