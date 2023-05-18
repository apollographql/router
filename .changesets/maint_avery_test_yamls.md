### Adds an integration test for all YAML configuration files in `./examples` ([Issue #2932](https://github.com/apollographql/router/issues/2932))

Adds an integration test that iterates over `./examples` looking for `.yaml` files that don't have a `Cargo.toml` or `.skipconfigvalidation` sibling file, and then running `setup_router_and_registry` on them, fast failing on any errors along the way.

By [@EverlastingBugstopper](https://github.com/EverlastingBugstopper) in https://github.com/apollographql/router/pull/3097
