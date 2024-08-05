### `apollo_router_cache_size` gauge sometimes missing ([PR #5770](https://github.com/apollographql/router/pull/5770))

This PR fixes the issue where the `apollo_router_cache_size` metric disappeared if the in memory cache was not mutated.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5770
