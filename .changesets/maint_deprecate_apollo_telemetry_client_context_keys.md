### Warn when deprecated `apollo_telemetry::client_name` / `apollo_telemetry::client_version` context keys are read ([Issue #ROUTER-1937](https://apollographql.atlassian.net/browse/ROUTER-1937))

The router now emits a `WARN`-level log message when the legacy 1.x context keys `apollo_telemetry::client_name` or `apollo_telemetry::client_version` are read as a fallback. Users relying on these keys (for example, via Rhai scripts or custom plugins) should migrate to the `apollo::telemetry::client_name` and `apollo::telemetry::client_version` keys. The fallback will be removed in a future 3.x release.

By [@carodewig](https://github.com/carodewig)
