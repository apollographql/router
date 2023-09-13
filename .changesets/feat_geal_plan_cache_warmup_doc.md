### Query plan cache warm-up improvements ([Issue #3704](https://github.com/apollographql/router/issues/3704))

The `warm_up_queries` option enables quicker schema updates by precomputing query plans for your most used cached queries and your persisted queries. When a new schema is loaded, a precomputed query plan for it may already be in in-memory cache.

We made a series of improvements to this feature to make it more usable:
* It is now active by default and warms up the cache with the 30% most used queries of the previous cache. The amount is still configurable, and it can be deactivated by setting it to 0.
* We added new metrics to track the time spent loading a new schema and planning queries in the warm-up phase. You can also measure the query plan cache usage, to know how many entries are used, and the cache hit rate, both for the in memory cache and the distributed cache.
* The warm-up will now plan queries in random order, to make sure that the work can be shared by multiple router instances using distributed caching
* Persisted queries are part of the warmed up queries.

You can get more information about operating the query plan cache and its warm-up phase in the [documentation](https://www.apollographql.com/docs/router/configuration/in-memory-caching#cache-warm-up)

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3815 https://github.com/apollographql/router/pull/3801 https://github.com/apollographql/router/pull/3767 https://github.com/apollographql/router/pull/3769 https://github.com/apollographql/router/pull/3770