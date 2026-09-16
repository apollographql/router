### Promote several `experimental_*` config options to stable names

Several config options have been promoted out of experimental. Rename them in
your router configuration:

| Old field | New field |
| --- | --- |
| `telemetry.exporters.tracing.experimental_response_trace_id` | `telemetry.exporters.tracing.response_trace_id` |
| `supergraph.experimental_log_on_broken_pipe` | `supergraph.log_on_broken_pipe` |
| `traffic_shaping.all.experimental_http2` | `traffic_shaping.all.http2` |
| `traffic_shaping.subgraphs.<name>.experimental_http2` | `traffic_shaping.subgraphs.<name>.http2` |
| `coprocessor.client.experimental_http2` | `coprocessor.client.http2` |

The `expose_query_plan` plugin has also been promoted. Its configuration
moves from the `plugins` map to a top-level key:

```yaml
# Before (no longer supported)
plugins:
  experimental.expose_query_plan: true

# After
expose_query_plan: true
```

Configurations using the `experimental` names will be migrated automatically
on startup during the 3.x version cycle.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/9945
