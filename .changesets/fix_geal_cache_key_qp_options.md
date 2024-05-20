### Prevent query plan cache collision when planning options change ([Issue #5093](https://github.com/apollographql/router/issues/5093))

The router's hashing algorithm has been updated to prevent cache collisions when the router's configuration changes.

> [!IMPORTANT]  
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

The router supports multiple options that affect the generated query plans, including:
* `defer_support`
* `generate_query_fragments`
* `experimental_reuse_query_fragments`
* `experimental_type_conditioned_fetching`
* `experimental_query_planner_mode`

If distributed query plan caching is enabled, changing any of these options results in different query plans being generated and cached.

This could be problematic in the following scenarios:

1. The router configuration changes and a query plan is loaded from cache which is incompatible with the new configuration.
2. Routers with different configurations share the same cache, which causes them to cache and load incompatible query plans. 

To prevent these from happening, the router now creates a hash for the entire query planner configuration and includes it in the cache key.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5100
