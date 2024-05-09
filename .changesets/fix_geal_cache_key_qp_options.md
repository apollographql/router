### Prevent query plan cache collision when planning options change ([Issue #5093](https://github.com/apollographql/router/issues/5093))

> [!IMPORTANT]  
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

When query planning takes place there are a number of options such as:
* `defer_support`
* `generate_query_fragments`
* `experimental_reuse_query_fragments`
* `experimental_type_conditioned_fetching`
* `experimental_query_planner_mode`

that will affect the generated query plans.

If distributed query plan caching is also enabled, then changing any of these will result in different query plans being generated and entering the cache.

This could cause issue in the following scenarios:
1. The Router configuration changes and a query plan is loaded from cache which is incompatible with the new configuration.
2. Routers with differing configuration are sharing the same cache causing them to cache and load incompatible query plans. 

Now a hash for the entire query planner configuration is included in the cache key to prevent this from happening.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5100
