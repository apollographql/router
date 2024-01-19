### Set log level to `info` by default for the entire application ([PR #4451](https://github.com/apollographql/router/pull/4451))

By default the log level of the application if `RUST_LOG` is not set is `info` and if you provide `--log` or `APOLLO_RUST_LOG` value then it overrides the default `info` log level with `apollo_router=...` because it only impacts the `apollo_router` crate, not external custom plugin and so one.

By doing this fix, by default if you have a custom plugin with info logs or metrics you won't have to enforce `RUST_LOG=info` everytime, it will work as expected.

> Note: it doesn't impact the behavior of `RUST_LOG` if you set it.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4451