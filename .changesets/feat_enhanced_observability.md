### New `apollo.router.cache.storage.estimated_size` gauge ([PR #5770](https://github.com/apollographql/router/pull/5770))

The router supports the new metric `apollo.router.cache.storage.estimated_size` that helps users understand and monitor the amount of memory that query planner cache entries consume.

The `apollo.router.cache.storage.estimated_size` metric gives an estimated size in bytes of a cache entry. It has the following attributes:
- `kind`: `query planner`.
- `storage`: `memory`.

Before using the estimate to decide whether to update the cache, users should validate that the estimate correlates with their pod's memory usage. 

To learn how to troubleshoot with this metric, see the [Pods terminating due to memory pressure](https://www.apollographql.com/docs/router/containerization/kubernetes#pods-terminating-due-to-memory-pressure) guide in docs.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5770