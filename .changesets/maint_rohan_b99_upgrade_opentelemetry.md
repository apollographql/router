### Upgrade OpenTelemetry to 0.33

The router now uses OpenTelemetry 0.33. Metric and span names, attributes and exporter output are unchanged, and OTLP exports are retried exactly as before.

Three edge cases behave differently:

- The OTLP compression environment variables (`OTEL_EXPORTER_OTLP_COMPRESSION` and its per-signal variants) are now read case-insensitively. An unrecognized value is ignored with a warning, falling back to the next variable or to no compression. Previously it stopped the exporter from being built.
- An incoming `tracestate` header with more than 32 entries keeps only the first 32, as the W3C Trace Context specification requires.
- An OTLP/HTTP export larger than 64 MiB is dropped and reported as an export error instead of being sent. Reduce `batch_processor.max_export_batch_size` if you see this error.

The router's configuration parser is also updated, with two fixes:

- A YAML integer above `9223372036854775807` keeps its exact value instead of being rounded to a floating-point number.
- Configuration error messages redact secrets in more places.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10426
