### feat(telemetry): add metrics for query plan warmup and schema loading ([Issue #3767](https://github.com/apollographql/router/issues/3767))

It adds histogram metrics for `query_planning_time` in warmup phase and `schema_loading_time`.

Example:

```
# HELP apollo_router_query_planning_time apollo_router_query_planning_time
# TYPE apollo_router_query_planning_time histogram
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="100"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 1
apollo_router_query_planning_time_bucket{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 1
apollo_router_query_planning_time_sum{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 0.022390619
apollo_router_query_planning_time_count{phase="warmup",service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 1
# HELP apollo_router_schema_loading_time apollo_router_schema_loading_time
# TYPE apollo_router_schema_loading_time histogram
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="100"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 8
apollo_router_schema_loading_time_bucket{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 8
apollo_router_schema_loading_time_sum{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 0.023486205999999996
apollo_router_schema_loading_time_count{service_name="apollo-router",otel_scope_name="apollo/router",otel_scope_version=""} 8
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3807
