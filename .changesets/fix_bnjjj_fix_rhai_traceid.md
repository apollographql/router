### Provide valid trace IDs for unsampled traces in Rhai scripts  ([PR #5606](https://github.com/apollographql/router/pull/5606))

The `traceid()` function in a Rhai script for the router now returns a valid trace ID for all traces. 

Previously, `traceid()` didn't return a valid trace ID if the trace wasn't sampled.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5606