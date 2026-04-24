### Add `stream_duration` instrument for streamed response lifecycle metrics ([PR #9269](https://github.com/apollographql/router/pull/9269))

Supergraph instruments now support a new `stream_duration` value that records the wall-clock time between when the supergraph response object is ready and when the response stream closes. This covers the full lifecycle of `@defer` and subscription responses — which `http.server.request.duration` does not capture, since that metric fires when the response object is prepared, before deferred chunks are emitted.

The instrument fires exactly once per request, on normal stream completion. Cancelled or errored streams do not emit a sample.

```yaml
telemetry:
  instrumentation:
    instruments:
      supergraph:
        acme.stream.duration:
          description: End-to-end time from first chunk to last chunk on streamed responses
          type: histogram
          unit: s
          value: stream_duration
```

Combine it with any supergraph attributes — including custom selectors — to segment the metric by operation, client, or whatever else you already use on other supergraph instruments.

By [@ebylund](https://github.com/ebylund) in https://github.com/apollographql/router/pull/9269
