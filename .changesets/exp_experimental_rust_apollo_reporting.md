### Experimental: Rust implementation of Apollo usage report field generation ([PR 4796](https://github.com/apollographql/router/pull/4796))

The router supports a new experimental Rust implementation for generating the stats report keys and referenced fields that are sent in Apollo usage reports. This implementation is one part of the effort to replace the router-bridge with native Rust code. 

The feature is configured with the `experimental_apollo_metrics_generation_mode` setting. We recommend that you use its default value, so we can verify that it generates the same payloads as the previous implementation.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/4796