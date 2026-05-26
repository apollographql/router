### Fix `is_primary_response` telemetry selector to correctly identify the primary response chunk ([PR #9238](https://github.com/apollographql/router/pull/9238))

The `is_primary_response: true` supergraph telemetry selector returned `false` for every chunk of a multipart `@defer` response — including the primary (first) chunk — when evaluated at `on_response` or `on_response_event` scope. This made it impossible for customers to distinguish primary from deferred chunks in metrics, events, and conditional telemetry.

The underlying `FIRST_EVENT_CONTEXT_KEY` context key was only set to `false` on the second-and-later chunks and was never set to `true` for the primary chunk. The selector requires the key to equal `Some(Bool(true))` to return `true`, so it always returned `false`.

This fix sets the key to `true` on the first chunk and `false` on subsequent chunks, so per-chunk filtering works as documented:

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
