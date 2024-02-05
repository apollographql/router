### Add configurable histogram buckets per metric ([Issue #4543](https://github.com/apollographql/router/issues/4543))

Add support for opentelemetry views (a mechanism to override instrument settings) for metrics. It will let you override default histogram buckets for example.

Example of configuration:

```yaml
telemetry:
  exporters:
    metrics:
      common:
        service_name: apollo-router
        views:
          - name: apollo_router_http_request_duration_seconds # Instrument name you want to edit. You can use wildcard in names. If you want to target all instruments just use '*'
            unit: "ms" # (Optional) override the unit
            description: "my new description of this metric" # (Optional) override the description
            aggregation: # (Optional)
              histogram:
                buckets: # Override default buckets configured for this histogram
                - 1
                - 2
                - 3
                - 4
                - 5
            allowed_attribute_keys: # (Optional) Keep only listed attributes on the metric
            - status
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4572