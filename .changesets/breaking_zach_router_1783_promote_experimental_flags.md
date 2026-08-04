### Promote several `experimental_*` config options to stable names

Several config options have been promoted out of experimental. Rename them in
your router configuration:

| Old field | New field |
| --- | --- |
| `telemetry.exporters.tracing.experimental_response_trace_id` | `telemetry.exporters.tracing.response_trace_id` |
| `supergraph.experimental_log_on_broken_pipe` | `supergraph.log_on_broken_pipe` |
| `supergraph.query_planning.experimental_plans_limit` | `supergraph.query_planning.plans_limit` |
| `supergraph.query_planning.experimental_paths_limit` | `supergraph.query_planning.paths_limit` |
| `traffic_shaping.all.experimental_http2` | `traffic_shaping.all.http2` |
| `traffic_shaping.subgraphs.<name>.experimental_http2` | `traffic_shaping.subgraphs.<name>.http2` |
| `coprocessor.client.experimental_http2` | `coprocessor.client.http2` |

The `expose_query_plan` plugin has also been promoted and is now registered
under the `apollo` namespace. Its configuration moves from the `plugins` map
to a top-level key:

```yaml
# Before (no longer supported)
plugins:
  experimental.expose_query_plan: true

# After
expose_query_plan: true
```

The internal `apollo.experimental_diagnostics` plugin has been renamed to
`apollo.diagnostics` (its configuration key changes from
`experimental_diagnostics` to `diagnostics`); this one has no migration path
since it isn't a documented, user-facing config surface.

Default values are unchanged. Configurations using the old field and plugin
names are migrated automatically at startup once the router reaches the next
major version.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/PR_NUMBER
