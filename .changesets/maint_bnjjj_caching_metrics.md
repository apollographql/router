### Add experimental caching metrics ([PR #3532](https://github.com/apollographql/router/pull/3532))

It adds a metric only if you configure `telemetry.metrics.common.experimental_cache_metrics.enabled` to `true`. It will generate metrics to evaluate which entities would benefit from caching. It simulates a cache with a TTL, configurable at `telemetry.metrics.common.experimental_cache_metrics.ttl` (default: 5 seconds), and measures the cache hit rate per entity type and subgraph.

example

```
# HELP apollo.router.operations.entity.cache_hit
# TYPE apollo_router_operations_entity.cache_hit histogram
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 3
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 4
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 4
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 4
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 4
apollo_router_operations_entity_cache_hitbucket{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 4
apollo_router_operations_entity_cache_hitsum{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version=""} 7
apollo_router_operations_entity_cache_hitcount{entity_type="Product",service_name="apollo-router",subgraph="products",otel_scope_name="apollo/router",otel_scope_version=""} 4
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 0
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 1
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 1
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 1
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 1
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 1
apollo_router_operations_entity_cache_hitbucket{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 1
apollo_router_operations_entity_cache_hitsum{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version=""} 1
apollo_router_operations_entity_cache_hitcount{entity_type="User",service_name="apollo-router",subgraph="users",otel_scope_name="apollo/router",otel_scope_version=""} 1
```

By [@bnjjj](https://github.com/bnjjj) [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/3532