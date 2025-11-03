### Add selectors for telemetry to create metrics based on cache-control values ([PR #8524](https://github.com/apollographql/router/pull/8524))

New selector `response_cache_control` added in telemetry for subgraph service to know what's the content of the computed [`Cache-Control` header](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cache-Control) from the subgraph response.

Example of attributes added to metrics:

```yaml
telemetry:
  apollo:
    field_level_instrumentation_sampler: 0.3
    errors:
      subgraph:
        all:
          redact: false
          send: true
  instrumentation:
    metrics:
      subgraph:
        http.client.request.duration:
          attributes:
            subgraph.name: true
            cache_control.max_age: # Value of max-age
              response_cache_control: max_age
            cache_control.public: # Is public data (from cache-control header)
              response_cache_control: public
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8524