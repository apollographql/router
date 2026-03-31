### Block router startup when certain OTEL environment variables are set ([PR #8915](https://github.com/apollographql/router/pull/8915))

The router now fails to start if any of the following OpenTelemetry (OTEL) environment variables are set:

- `OTEL_EXPORTER_OTLP_ENDPOINT`
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
- `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`

Using these variables isn't supported by the router because they can override or interfere with its built-in telemetry configuration, leading to unexpected behavior.

Previously, the router emitted a warning when `OTEL_EXPORTER_OTLP_ENDPOINT` was present. Startup is now blocked to prevent unintended telemetry configuration conflicts.

If your deployment defines any of these environment variables (for example, through base container images, platform defaults, or infrastructure tooling), remove them before starting the router.

By [@OriginLeon](https://github.com/OriginLeon) in https://github.com/apollographql/router/pull/8915
