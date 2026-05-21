### Add `is_deferred` subgraph telemetry selector ([PR #9262](https://github.com/apollographql/router/pull/9262))

A new `is_deferred: true` selector is now available on the `subgraph` service of the telemetry instrumentation config. It returns `true` when the current subgraph fetch is part of the deferred portion of an `@defer` query plan, and `false` for primary (non-deferred) fetches.

This lets you split subgraph metrics like `http.client.request.duration` between primary and deferred fetches — useful for seeing whether deferred fetches contribute disproportionately to tail latency on `@defer`-adopting operations:

```yaml
telemetry:
  instrumentation:
    instruments:
      subgraph:
        http.client.request.duration:
          attributes:
            phase:
              is_deferred: true
```

Produces two series: `http_client_request_duration_seconds{phase="true"}` (deferred fetches) and `http_client_request_duration_seconds{phase="false"}` (primary fetches).

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9262
