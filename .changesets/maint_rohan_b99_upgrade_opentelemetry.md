### Upgrade OpenTelemetry to 0.33

The router now uses OpenTelemetry 0.33. Metric and span names, attributes and exporter output are unchanged, and the OTLP exporters still send each export once, without retrying.

Two edge cases behave differently:

- The OTLP compression environment variables (`OTEL_EXPORTER_OTLP_COMPRESSION` and its per-signal variants) are now read case-insensitively. An unrecognized value is ignored with a warning, falling back to the next variable or to no compression. Previously it stopped the exporter from being built.
- An incoming `tracestate` header with more than 32 entries keeps only the first 32, as the W3C Trace Context specification requires.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10426
