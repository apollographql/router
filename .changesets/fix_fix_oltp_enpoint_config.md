### Use correct default values on omitted OTLP endpoints ([PR #6931](https://github.com/apollographql/router/pull/6931))

Previously, when the configuration didn't specify an OTLP endpoint, the Router would always default to `http://localhost:4318`. However, port `4318` is the correct default only for the HTTP protocol, while port `4317` should be used for gRPC.

Additionally, all other telemetry defaults in the Router configuration consistently use `127.0.0.1` as the hostname rather than `localhost`.

With this change, the Router now uses:
* `http://127.0.0.1:4317` as the default for gRPC protocol
* `http://127.0.0.1:4318` as the default for HTTP protocol

This ensures protocol-appropriate port defaults and consistent hostname usage across all telemetry configurations.

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6931