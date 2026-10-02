### Deprecate `apollo.router.session.count.active` metric

The `apollo.router.session.count.active` up/down counter is now marked deprecated. Its exported metric description directs operators to the OpenTelemetry-compliant replacement `http.server.active_requests`, and the router additionally logs a deprecation warning at startup. The metric continues to be emitted under its current name for backward compatibility, but may be removed in a future release.

Note: `http.server.active_requests` is enabled by default when `telemetry.instrumentation.instruments.default_requirement_level` is `required` or `recommended` (the default). Operators who have explicitly set `default_requirement_level: none` will need to enable it manually in their telemetry config.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9541
