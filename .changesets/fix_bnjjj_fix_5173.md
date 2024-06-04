### Metrics overflow is logged as a warning rather than error ([Issue #5173](https://github.com/apollographql/router/issues/5173))

If a metric has too high a cardinality the following warning will be displayed as a warning instead of an errorl:

`OpenTelemetry metric error occurred: Metrics error: Warning: Maximum data points for metric stream exceeded/ Entry added to overflow`

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5287