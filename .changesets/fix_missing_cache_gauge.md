### Missing cache gauge ([PR #5770](https://github.com/apollographql/router/pull/5770))

Query planner cache entries may use an significant amount of memory in the Router.
To help users understand and monitor this the Router now exposes a new metric `apollo.router.cache.storage.estimated_size`.

This metric has the following attributes:
- `kind`: `query planner`.
- `storage`: `memory`.

As the size is only an estimation, users should check for correlation with pod memory usage to determine if cache needs to be updated.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5770
