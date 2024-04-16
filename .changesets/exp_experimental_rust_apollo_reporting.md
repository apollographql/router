### Experimental implementation of Apollo usage report field generation ([PR 4796](https://github.com/apollographql/router/pull/4796))

This adds a new and experimental Rust implementation of the generation of the stats report key and referenced fields that are sent in Apollo usage reports, as part of the effort to replace the router-bridge with native Rust code. For now, we recommend that the `experimental_apollo_metrics_generation_mode` setting should be left at the default value while we confirm that it generates identical payloads to router-bridge.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/4796