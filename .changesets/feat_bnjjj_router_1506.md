### Add telemetry selector for Cache-Control metrics ([PR #8524](https://github.com/apollographql/router/pull/8524))

The new `response_cache_control` selector enables telemetry metrics based on the computed [`Cache-Control` header](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cache-Control) from subgraph responses.

**Example configuration:**

```yaml
telemetry:
  exporters:
    metrics:
      common:
        service_name: apollo-router
        views:
          - name: subgraph.response.cache_control.max_age
            aggregation:
              histogram:
                buckets:
                - 10
                - 100
                - 1000
                - 10000
                - 100000
  instrumentation:
    instruments:
      subgraph:
        subgraph.response.cache_control.max_age:
          value:
            response_cache_control: max_age
          type: histogram
          unit: s
          description: A histogram of the computed TTL for a subgraph response
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8524
