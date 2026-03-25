### Add retry layer for push metrics exporters ([PR #9036](https://github.com/apollographql/router/pull/9036))

Add `RetryMetricExporter`, which will retry up to 3 times with jittered exponential backoff to the `apollo metrics` and `otlp` named exporters.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9036
