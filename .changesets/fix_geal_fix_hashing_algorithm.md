### Fix cache key hashing algorithm ([Issue #5160](https://github.com/apollographql/router/issues/5160))

> [!IMPORTANT]
> If you have enabled [Distributed query plan caching](https://www.apollographql.com/docs/router/configuration/distributed-caching/#distributed-query-plan-caching), this release changes the hashing algorithm used for the cache keys.  On account of this, you should anticipate additional cache regeneration cost when updating between these versions while the new hashing algorithm comes into service.

The Router contains a schema aware query hashing algorithm, designed to return the same hash across schema updates if they do not affect the query. It was used mainly to avoid planning a lot of queries when warming up the query plan cache. It was deactivated due to a regression. This reintroduces this algorithm, with a serie of reliability and performance fixes, along with better observability.

The hashing algorithm can be activated in configuration, if query plan cache warm up is already enabled:

```yaml title="router.yaml"
supergraph:
  query_planning:
    warmed_up_queries: 100
    experimental_reuse_query_plans: true
```

There is a counter metric named `apollo.router.query.planning.warmup.reused` that can be used to track the hashing algorithm benefits:
- if the `experimental_reuse_query_plans` option is false, the `query_plan_reuse_active` metric attribute will be false. Cache warm up will not reuse query plans according to the algorithm, but it will evaluate if some of them could have been reused and report that in the metric
- if the `experimental_reuse_query_plans` option is true, then the `query_plan_reuse_active` metric attribute will be true

Fixes included in this change:
- strenghten the hashing algorithm to prevent collisions. In particular, the query string is always hashed in, making sure that different queries cannot get the same hash
- remove inefficiencies in cache key generation
- use prefixes for each part of the Redis cache key, so they become self describing

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5255
