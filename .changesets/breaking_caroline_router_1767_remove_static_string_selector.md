### Remove deprecated `Static(String)` telemetry selector variant

The deprecated `Static(String)` variant has been removed from the `supergraph`, `router`, and `subgraph` telemetry selector enums. Use the `static` field instead:

```yaml
# Before (no longer supported)
attributes:
  my.attr: "my-value"

# After
attributes:
  my.attr:
    static: "my-value"
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/XXXX
