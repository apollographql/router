### Improve performance, don't re-create meter and instruments on every calls in Telemetry ([PR #5629](https://github.com/apollographql/router/pull/5629))

The creation of otel instruments using a regex is no longer part of the hot path. Now we create these instruments when starting the telemetry plugin and not in every serives.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5629