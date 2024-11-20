### Query planner cache key improvements ([Issue #5160](https://github.com/apollographql/router/issues/5160))

> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

This brings several performance improvements to the query plan cache key generation. In particular, it changes the distributed cache's key format, adding prefixes to the different key segments, to help in debugging.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6206