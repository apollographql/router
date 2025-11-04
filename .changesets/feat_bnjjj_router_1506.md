### Add selectors for telemetry to create metrics based on cache-control values ([PR #8524](https://github.com/apollographql/router/pull/8524))

New selector `response_cache_control` added in telemetry for subgraph service to know what's the content of the computed [`Cache-Control` header](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cache-Control) from the subgraph response.

Example of attributes added to metrics:

```yaml
telemetry:
  exporters:
    metrics:
      common:
        service_name: apollo-router
        views:
          - name: subgraph.response.cache_control.max_age # This is to make sure it will use the correct buckets for the max age histogram
            aggregation:
              histogram:
                buckets: # Override default buckets configured for this histogram
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
          unit: s # Seconds
          description: A histogram of the computed TTL for a subgraph response
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8524
