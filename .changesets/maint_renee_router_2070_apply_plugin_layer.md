### Ordering change in subgraph telemetry ([PR #10074](https://github.com/apollographql/router/pull/10074))

Custom instrumentation at the subgraph level is now executed immediately when the span is created. In practice, this means that custom instrumentation can no longer use the `apollo-federation-include-trace` header on subgraph requests.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/10074