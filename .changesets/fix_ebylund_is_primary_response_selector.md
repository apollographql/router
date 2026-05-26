### Identify the primary `@defer` chunk correctly in the `is_primary_response` telemetry selector ([PR #9238](https://github.com/apollographql/router/pull/9238))

The `is_primary_response: true` supergraph telemetry selector returned `false` for every chunk of a multipart `@defer` response — including the primary (first) chunk — when evaluated at `on_response` or `on_response_event` scope.  This made it impossible to distinguish primary from deferred chunks in metrics, events, and conditional telemetry.

The selector now returns `true` for the primary chunk and `false` for subsequent deferred chunks, so per-chunk filtering works as documented:

```yaml
telemetry:
  instrumentation:
    instruments:
      supergraph:
        my.defer.primary.duration:
          value: event_duration
          type: histogram
          attributes:
            is_primary:
              is_primary_response: true
```

Now produces split metric series (`is_primary="true"` for the primary chunk, `is_primary="false"` for deferred chunks) instead of a single series with `is_primary="false"` for everything.

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9238
