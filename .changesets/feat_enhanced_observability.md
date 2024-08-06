### `apollo.router.cache.storage.estimated_size` gauge ([PR #5770](https://github.com/apollographql/router/pull/5770))

Query planner cache entries may use an significant amount of memory in the Router.
To help users understand and monitor this the Router now exposes a new metric `apollo.router.cache.storage.estimated_size`.

This metric give an estimated size in bytes for the cache entry and has the following attributes:
- `kind`: `query planner`.
- `storage`: `memory`.

As the size is only an estimation, users should check for correlation with pod memory usage to determine if cache needs to be updated.

Usage scenario:
* Your pods are being terminated due to memory pressure.
* Add the following metrics to your monitoring system to track:
  * `apollo.router.cache.storage.estimated_size`.
  * `apollo_router_cache_size`.
  * ratio of `apollo_router_cache_hits` - `apollo_router_cache_misses`.

* Observe the `apollo.router.cache.storage.estimated_size` to see if it grows over time and correlates with pod memory usage.
* Observe the ratio of cache hits to misses to determine if the cache is being effective.

Remediation:
* Adjust the cache size to lower if the cache reaches near 100% hit rate but the cache size is still growing.
* Increase the pod memory to higher if cache hit rate is low and cache size is still growing.
* Adjust the cache size to lower if the latency of query planning cache misses is acceptable and memory availability is limited.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5770