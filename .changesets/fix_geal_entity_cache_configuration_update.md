### Entity cache: require the presence of a Cache-Control header ([Issue #4880](https://github.com/apollographql/router/issues/4880))

The entity cache plugin intended to require a `Cache-Control` header from the subgraph to decide whether or not a response should be cached. Unfortunately in the way tit was set up, all responses were stored.
The plugin now makes sure that the `Cache-Control` is there, and if a subgraph does not provide it, then the aggregated `Cache-Control` header sent to the client will contain `no-store`.

Additionally, the Router will now check that a TTL is configured for all subgraphs, either in per subgraph configuration, or globally.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4882