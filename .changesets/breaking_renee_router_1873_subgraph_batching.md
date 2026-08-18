### Remove special-case subgraph batching span ([PR #9978](https://github.com/apollographql/router/pull/9978))

Subgraph telemetry behaves differently when subgraph batching is enabled. In Router v2.x, batched subgraph requests would produce a single subgraph span with a special "batch" value for the `graphql.operation.name` attribute. Starting in Router 3, requests are instead only batched up at the HTTP level. This means that each subgraph request that is part of a batch emits its own full-fledged span, but only one HTTP client span is emitted for the entire batch.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/9978