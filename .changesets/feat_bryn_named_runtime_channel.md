### Improve BatchProcessor observability ([Issue #6558](https://github.com/apollographql/router/issues/6558))

A new metric has been introduced to allow observation of how many spans are being dropped by an telemetry batch processor.

- `apollo.router.telemetry.batch_processor.errors` - The number of errors encountered by exporter batch processors.
    - `name`: One of `apollo-tracing`, `datadog-tracing`, `jaeger-collector`, `otlp-tracing`, `zipkin-tracing`. 
    - `error` = One of `channel closed`, `channel full`.

By observing the number of spans dropped it is possible to estimate what batch processor settings will work for you.

In addition, the log message for dropped spans will now indicate which batch processor is affected.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/6558
