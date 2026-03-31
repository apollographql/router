### Add retry layer for push metrics exporters ([PR #9036](https://github.com/apollographql/router/pull/9036))

Adds a `RetryMetricExporter` layer that retries up to three times with jittered exponential backoff for the `apollo metrics` and `otlp` named exporters.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9036
