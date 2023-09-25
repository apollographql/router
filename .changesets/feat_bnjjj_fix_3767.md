### feat(telemetry): add metrics for query plan warmup and schema load ([Issue #3767](https://github.com/apollographql/router/issues/3767))

It adds histogram metrics for `apollo_router_query_planning_warmup_duration` and `apollo_router_schema_load_duration`.

Example in Prometheus:

```
# HELP apollo_router_query_planning_warmup_duration apollo_router_query_planning_warmup_duration
# TYPE apollo_router_query_planning_warmup_duration histogram
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="100"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 1
apollo_router_query_planning_warmup_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 1
apollo_router_query_planning_warmup_duration_sum{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 0.022390619
apollo_router_query_planning_warmup_duration_count{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 1
# HELP apollo_router_schema_load_duration apollo_router_schema_load_duration
# TYPE apollo_router_schema_load_duration histogram
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="100"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 8
apollo_router_schema_load_duration_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 8
apollo_router_schema_load_duration_sum{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 0.023486205999999996
apollo_router_schema_load_duration_count{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 8
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3807
