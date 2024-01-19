### Set application default log level to `info` ([PR #4451](https://github.com/apollographql/router/pull/4451))

This release sets the default log level to `info` for an entire application, including custom external plugins, when the [`RUST_LOG` environment variable](https://www.apollographql.com/docs/router/configuration/telemetry/exporters/logging/overview/#log-level) isn't set.

Previously, if you set the `--log` command-line option or `APOLLO_RUST_LOG` environment variable, their log level setting impacted more than the `apollo_router` crate and caused custom plugins with `info` logs or metrics to have to manually set `RUST_LOG=info`.

> Note: setting `RUST_LOG` changes the application log level.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4451