### Warn at startup when `OTEL_EXPORTER_OTLP_ENDPOINT` is set ([PR #8729](https://github.com/apollographql/router/pull/8729))

The router now displays a warning at startup if the `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable is set. This variable takes precedence over default configurations and can override trace export to Apollo Studio, so the warning helps you identify when telemetry data might not be sent where expected.

By [@apollo-mateuswgoettems](https://github.com/apollo-mateuswgoettems) in https://github.com/apollographql/router/pull/8729