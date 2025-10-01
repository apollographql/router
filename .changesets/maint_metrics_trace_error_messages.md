### Trace and metrics exporter wrappers to append details to errors ([PR #8363](https://github.com/apollographql/router/pull/8363))

Added a wrapper around `MetricsExporter`s and `SpanExporter`s so that the exporter type can be appended to error messages. This allows us to differentiate between e.g. "Apollo OTel" errors and "OTLP exporter" errors.

By [@bonnici](https://github.com/bonnici) in https://github.com/apollographql/router/pull/8363
