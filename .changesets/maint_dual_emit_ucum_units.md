### Dual-emit duration and byte metrics under their UCUM-suffixed Prometheus names

Several existing router metrics are gaining a UCUM unit (`s` for durations, `By` for byte counts). The `opentelemetry-prometheus` exporter auto-appends a suffix derived from the unit, so the rename is customer-visible — e.g. `apollo_router_cache_hit_time` becomes `apollo_router_cache_hit_time_seconds`.

To give dashboards and alerts a migration window, the router now emits each affected metric under **both** names simultaneously:

- The legacy unsuffixed name continues to appear on `otel_scope_name="apollo/router"` exactly as before.
- A new suffixed series appears on `otel_scope_name="apollo/router/ucum"` with the future-canonical UCUM unit set.

Affected metrics:

| Legacy Prometheus name | Future Prometheus name (canonical) |
| --- | --- |
| `apollo_router_cache_hit_time` | `apollo_router_cache_hit_time_seconds` |
| `apollo_router_cache_invalidation_duration` | `apollo_router_cache_invalidation_duration_seconds` |
| `apollo_router_cache_miss_time` | `apollo_router_cache_miss_time_seconds` |
| `apollo_router_operations_coprocessor_duration` | `apollo_router_operations_coprocessor_duration_seconds` |
| `apollo_router_query_planning_plan_duration` | `apollo_router_query_planning_plan_duration_seconds` |
| `apollo_router_query_planning_total_duration` | `apollo_router_query_planning_total_duration_seconds` |
| `apollo_router_query_planning_warmup_duration` | `apollo_router_query_planning_warmup_duration_seconds` |
| `apollo_router_schema_load_duration` | `apollo_router_schema_load_duration_seconds` |
| `apollo_router_uplink_fetch_duration_seconds` | `apollo_router_uplink_fetch_duration_seconds` (output unchanged; OTel name changes from `apollo.router.uplink.fetch.duration.seconds` to `apollo.router.uplink.fetch.duration`) |
| `apollo_router_operations_fetch_request_size_total` | `apollo_router_operations_fetch_request_size_bytes_total` |
| `apollo_router_operations_fetch_response_size_total` | `apollo_router_operations_fetch_response_size_bytes_total` |
| `apollo_router_operations_request_size_total` | `apollo_router_operations_request_size_bytes_total` |
| `apollo_router_operations_response_size_total` | `apollo_router_operations_response_size_bytes_total` |
| `apollo_router_operations_file_uploads_file_size` | `apollo_router_operations_file_uploads_file_size_bytes` |

**Action recommended:** update dashboards, alerts, and recording rules to the new suffixed names during this router 3.x dual-emit window. The legacy unsuffixed names will be removed in a future major version.

**Cardinality note:** each affected metric now produces two series per attribute combination — distinguishable by the `otel_scope_name` label. The legacy series can be filtered out via `{otel_scope_name="apollo/router/ucum"}` in queries once dashboards have migrated.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9633
