### Fix tracing default service name ([Issue #2641](https://github.com/apollographql/router/issues/2641))

The default tracing service name should be `router`. At some point expansion by defaults was changed, which has lead to the default being `${env.OTEL_SERVICE_NAME:-router}`.

This new default was expanded, and the resulting service name in tracing tools is unpleasant.

`telemetry.tracing.trace_config.service_name` is now defaulted to `router` again.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2642
