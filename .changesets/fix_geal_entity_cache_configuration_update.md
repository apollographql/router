### Require Cache-Control header for entity cache ([Issue #4880](https://github.com/apollographql/router/issues/4880))

Previously, the router's entity cache plugin didn't use a subgraph's `Cache-Control` header to decide whether to store a response. Instead, it cached all responses. 

Now, the router's entity cache plugin expects a `Cache-Control` header from a subgraph. If a subgraph does not provide it, the aggregated `Cache-Control` header sent to the client will contain `no-store`.

Additionally, the router now verifies that a TTL is configured for all subgraphs, either globally or for each subgraph configuration.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4882