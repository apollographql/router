### Add configurable histogram buckets per metric ([Issue #4543](https://github.com/apollographql/router/issues/4543))

Add support for opentelemetry views (a mechanism to override instrument settings) for metrics. It will let you override default histogram buckets for example.

Example of configuration:

```yaml
telemetry:
  apollo:
    client_name_header: name_header
    client_version_header: version_header
  exporters:
    metrics:
      common:
        service_name: apollo-router
        views:
          - instrument_name: apollo_router_http_request_duration_seconds
            aggregation:
              histogram:
                buckets:
                - 1
                - 2
                - 3
                - 4
                - 5
      prometheus:
        enabled: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4572