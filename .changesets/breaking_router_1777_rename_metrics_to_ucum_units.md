### Rename duration and byte-count metrics to their UCUM-suffixed Prometheus names

Several existing router metrics have gained a UCUM unit (`s` for durations, `By` for byte counts). The `opentelemetry-prometheus` exporter auto-appends a suffix derived from the unit, so this rename is customer-visible — e.g. `apollo_router_cache_hit_time` becomes `apollo_router_cache_hit_time_seconds`.

Renamed metrics (Prometheus names):

| Legacy name | New name |
| --- | --- |
| `apollo_router_cache_hit_time` | `apollo_router_cache_hit_time_seconds` |
| `apollo_router_cache_miss_time` | `apollo_router_cache_miss_time_seconds` |
| `apollo_router_operations_coprocessor_duration` | `apollo_router_operations_coprocessor_duration_seconds` |
| `apollo_router_query_planning_plan_duration` | `apollo_router_query_planning_plan_duration_seconds` |
| `apollo_router_query_planning_total_duration` | `apollo_router_query_planning_total_duration_seconds` |
| `apollo_router_query_planning_warmup_duration` | `apollo_router_query_planning_warmup_duration_seconds` |
| `apollo_router_schema_load_duration` | `apollo_router_schema_load_duration_seconds` |
| `apollo_router_uplink_fetch_duration_seconds` | `apollo_router_uplink_fetch_duration_seconds` (Prometheus output unchanged; the underlying OTel metric name changes from `apollo.router.uplink.fetch.duration.seconds` to `apollo.router.uplink.fetch.duration`, which matters for OTLP consumers) |
| `apollo_router_operations_fetch_request_size_total` | `apollo_router_operations_fetch_request_size_bytes_total` |
| `apollo_router_operations_fetch_response_size_total` | `apollo_router_operations_fetch_response_size_bytes_total` |
| `apollo_router_operations_request_size_total` | `apollo_router_operations_request_size_bytes_total` |
| `apollo_router_operations_response_size_total` | `apollo_router_operations_response_size_bytes_total` |
| `apollo_router_operations_file_uploads_file_size` | `apollo_router_operations_file_uploads_file_size_bytes` |

**Action required:** update dashboards, alerts, and recording rules that reference the legacy names above. See the [router 3.x upgrade guide](https://www.apollographql.com/docs/graphos/routing/upgrade/from-router-v2#renamed-prometheus-metrics) for the full mapping.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9633
