### Warm query plan cache using persisted queries on startup ([Issue #5334](https://github.com/apollographql/router/issues/5334))

Adds support for the router to use [persisted queries](https://www.apollographql.com/docs/graphos/operations/persisted-queries/) to warm the query plan cache upon startup using a new `experimental_prewarm_query_plan_cache` configuration option under `persisted_queries`.

To enable:

```yml
persisted_queries:
  enabled: true
  experimental_prewarm_query_plan_cache: true
```

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/5340
