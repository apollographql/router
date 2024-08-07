### `apollo.router.cache.storage.estimated_size` gauge ([PR #5770](https://github.com/apollographql/router/pull/5770))

Query planner cache entries may use an significant amount of memory in the Router.
To help users understand and monitor this the Router now exposes a new metric `apollo.router.cache.storage.estimated_size`.

This metric give an estimated size in bytes for the cache entry and has the following attributes:
- `kind`: `query planner`.
- `storage`: `memory`.

Before using the estimate to decide whether to update the cache, users should validate that the estimate correlates with their pod's memory usage. 

To learn how to troubleshoot with this metric, see the [Pods terminating due to memory pressure](https://www.apollographql.com/docs/router/containerization/kubernetes#pods-terminating-due-to-memory-pressure) guide in docs.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5770