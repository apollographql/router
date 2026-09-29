### Nested telemetry settings reject unknown keys

These telemetry blocks used to ignore keys they didn't define:

- `batch_processor` under `telemetry.exporters.tracing.otlp`, `telemetry.exporters.tracing.datadog`, `telemetry.exporters.metrics.otlp` and `telemetry.apollo.tracing`
- `telemetry.apollo.metrics.otlp.batch_processor` and `telemetry.apollo.metrics.usage_reports.batch_processor`

They now reject unknown keys, the same as the rest of the telemetry configuration. A typo such as `batch_processor.max_queue_sise` now fails when the configuration loads, instead of the setting silently keeping its default. The configuration JSON Schema marks these blocks `additionalProperties: false`.

To migrate, run `router config validate` and correct or remove any unknown key it reports in these blocks.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/####
