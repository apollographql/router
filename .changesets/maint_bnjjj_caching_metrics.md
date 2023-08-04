### Add experimental caching metrics ([PR #3532](https://github.com/apollographql/router/pull/3532))

It adds a metric only if you configure `telemetry.metrics.common.experimental_enable_cache_metrics` to `true`. It will generate metrics to better know where and which data would benefit from caching.

example

```
# HELP apollo_router_operations_entity_cachable apollo.router.operations.entity.cachable
# TYPE apollo_router_operations_entity_cachable histogram
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 3
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 4
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 4
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 4
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 4
apollo_router_operations_entity_cachable_bucket{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 4
apollo_router_operations_entity_cachable_sum{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version=""} 7
apollo_router_operations_entity_cachable_count{entity_type="Product",service_name="apollo-router",subgraph="products",vary="",otel_scope_name="apollo/router",otel_scope_version=""} 4
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.05"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.1"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.25"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="0.5"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="1"} 0
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="2.5"} 1
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="5"} 1
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="10"} 1
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="20"} 1
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="1000"} 1
apollo_router_operations_entity_cachable_bucket{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version="",le="+Inf"} 1
apollo_router_operations_entity_cachable_sum{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version=""} 1
apollo_router_operations_entity_cachable_count{entity_type="User",service_name="apollo-router",subgraph="users",vary="",otel_scope_name="apollo/router",otel_scope_version=""} 1
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3532