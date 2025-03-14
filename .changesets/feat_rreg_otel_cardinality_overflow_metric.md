### Metric to Measure Cardinality Overflow Frequency ([PR #6998](https://github.com/apollographql/router/pull/6998))

Adds a new counter metric that is incremented when the [cardinality overflow log](https://github.com/open-telemetry/opentelemetry-rust/blob/d583695d30681ee1bd910156de27d91be3711822/opentelemetry-sdk/src/metrics/internal/mod.rs#L134) from [opentelemetry-rust](https://github.com/open-telemetry/opentelemetry-rust) occurs. This log means that a metric in a batch has reached a cardinality of > 2000 and that any excess attributes will be ignored.

By [@rregitsky](https://github.com/rregitsky) in https://github.com/apollographql/router/pull/6998
