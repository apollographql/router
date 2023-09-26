### Query plan cache warm-up improvements. ([Issue #3704](https://github.com/apollographql/router/issues/3704), [Issue #3767](https://github.com/apollographql/router/issues/3767))

The `warm_up_queries` option enables quicker schema updates by precomputing query plans for your most used cached queries and your persisted queries. When a new schema is loaded, a precomputed query plan for it may already be in the in-memory cache.

We made a series of improvements to this feature to make it easier to use:
* It is now active by default.
* It warms up the cache with the 30% most used queries from previous cache.
* The query cache percentage continues to be configurable, and it can be deactivated by setting it to 0.
* The warm-up will now plan queries in random order to make sure that the work can be shared by multiple router instances using distributed caching.
* Persisted queries are part of the warmed up queries.

We also added histogram metrics for `apollo_router_query_planning_warmup_duration` and `apollo_router_schema_load_duration`. These metrics make it easier to track the time spent loading a new schema and planning queries in the warm-up phase. You can measure the query plan cache usage for both the in-memory-cache and distributed cache. This makes it easier to know how many entries are used as well as the cache hit rate.

Here is what these metrics would look like in Prometheus:

```
# HELP apollo_router_query_planning_warmup_duration apollo_router_query_planning_warmup_duration
# TYPE apollo_router_query_planning_warmup_duration histogram
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 1
apollo_router_query_planning_warmup_duration_sum{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 0.022390619
apollo_router_query_planning_warmup_duration_count{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 1
# HELP apollo_router_schema_load_duration apollo_router_schema_load_duration
# TYPE apollo_router_schema_load_duration histogram
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 8
```

You can get more information about operating the query plan cache and its warm-up phase in the [documentation](https://www.apollographql.com/docs/router/configuration/in-memory-caching#cache-warm-up)

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3815 https://github.com/apollographql/router/pull/3801 https://github.com/apollographql/router/pull/3767 https://github.com/apollographql/router/pull/3769 https://github.com/apollographql/router/pull/3770

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3807