### Fix missing `apollo_router_cache_size` metric ([PR #5770](https://github.com/apollographql/router/pull/5770))

Previously, if the in-memory cache wasn't mutated, the `apollo_router_cache_size` metric wouldn't be available. This has been fixed in this release.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5770
