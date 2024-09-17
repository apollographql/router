### Gauges stop working after hot reload ([PR #5996](https://github.com/apollographql/router/pull/5996), [PR #5999](https://github.com/apollographql/router/pull/5999), [PR #5999](https://github.com/apollographql/router/pull/6012))

When the router reloads the schema or config, some gauges stopped working. These were:
* `apollo.router.cache.storage.estimated_size`
* `apollo_router_cache_size`
* `apollo.router.v8.heap.used`
* `apollo.router.v8.heap.total`
* `apollo.router.query_planning.queued`

The gauges will now continue to function after a router hot reload. 

As a result of this change, introspection queries will now share the same cache even when query planner pooling is used.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5996 and https://github.com/apollographql/router/pull/5999 and https://github.com/apollographql/router/pull/6012
