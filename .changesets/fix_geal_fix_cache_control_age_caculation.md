### Entity cache: fix Cache-Control aggregation and age calculation ([PR #5463](https://github.com/apollographql/router/pull/5463))

This ensures the `max-age` or `s-max-age` fields of the `Cache-Control` header returned to the client are calculated properly, and proper default values are set if a subgraph does not send back a `Cache-Control` header. This also makes sure the header is always aggregated, even if the plugin is disabled entirely or for a specific subgraph.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5463