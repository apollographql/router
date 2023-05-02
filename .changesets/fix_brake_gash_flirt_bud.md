### Limit the memory usage of apollo telemetry exporter ([PR #3006](https://github.com/apollographql/router/pull/3006))

Add a new LRU cache instead of a Vec for sub span data and do not keep in memory all events for a span because we don't need it for our computations.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3006