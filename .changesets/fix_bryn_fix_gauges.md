### Fix stopped gauges upon hot reload ([PR #5996](https://github.com/apollographql/router/pull/5996), [PR #5999](https://github.com/apollographql/router/pull/5999), [PR #5999](https://github.com/apollographql/router/pull/6012))

Previously when the router hot-reloaded a schema or a configuration file, the following gauges stopped working:

* `apollo.router.cache.storage.estimated_size`
* `apollo_router_cache_size`
* `apollo.router.v8.heap.used`
* `apollo.router.v8.heap.total`
* `apollo.router.query_planning.queued`

This issue has been fixed in this release, and the gauges now continue to function after a router hot-reloads. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5996 and https://github.com/apollographql/router/pull/5999 and https://github.com/apollographql/router/pull/6012
