### Query plan cache gauges stop working after hot reload ([PR #5996](https://github.com/apollographql/router/pull/5996))

When the router reloads the schema or config, the query plan cache gauges stop working. These are:
* `apollo.router.cache.storage.estimated_size`
* `apollo_router_cache_size`

The gauges will now continue to function after a router hot reload. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5996
