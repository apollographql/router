### Reuse cached query plans across schema updates ([Issue #4834](https://github.com/apollographql/router/issues/4834))

The router now supports an experimental feature to reuse schema aware query hashing—introduced with the [entity caching](https://www.apollographql.com/docs/router/configuration/entity-caching/) feature—to cache query plans. It reduces the amount of work when reloading the router. The hash of the cache stays the same for a query across schema updates if the schema updates don't change the query. If query planner [cache warm-up](https://www.apollographql.com/docs/router/configuration/in-memory-caching/#cache-warm-up) is configured, the router can reuse previous cache entries for which the hash does not change, consequently reducing both CPU usage and reload duration.

You can enable reuse of cached query plans by setting the `supergraph.query_planning.experimental_reuse_query_plans` option:

```yaml title="router.yaml"
supergraph:
  query_planning:
    warmed_up_queries: 100
    experimental_reuse_query_plans: true
```

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4883