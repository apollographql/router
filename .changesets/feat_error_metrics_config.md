### Changes to experimental error metrics ([PR #6966](https://github.com/apollographql/router/pull/6966))

In 2.0.0, an experimental metric `telemetry.apollo.errors.experimental_otlp_error_metrics` was introduced to track errors with additional attributes. A few related changes are included here:

- Sending these metrics now also respects the subgraph's `send` flag e.g. `telemetry.apollo.errors.subgraph.[all|(subgraph name)].send`.
- A new configuration option `telemetry.apollo.errors.subgraph.[all|(subgraph name)].redaction_policy` has been added. This flag only applies when `redact` is set to `true`. When set to `ErrorRedactionPolicy.Strict`, error redaction will behave as it has in the past. Setting this to `ErrorRedactionPolicy.Extended` will allow the `extensions.code` value from subgraph errors to pass through redaction and be sent to Studio.
- A warning about incompatibility of error telemetry with connectors will be suppressed when this feature is enabled, since it _does_ support connectors when using the new mode.

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/6966
