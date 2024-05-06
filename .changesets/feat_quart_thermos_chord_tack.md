### Support conditions on custom attributes for spans and a new selector for GraphQL errors ([Issue #4336](https://github.com/apollographql/router/issues/4336))

The router now supports conditionally adding attributes on a span and the new `on_graphql_error` selector that is set to true if the response body contains GraphQL errors.

An example configuration using `condition` in `attributes` and `on_graphql_error`:

```yaml
telemetry:
  instrumentation:
    spans: 
      router: 
        attributes:    
          otel.status_description: 
            static: "there was an error"
            condition:
              any:
              - not:
                  eq:
                  - response_status: code
                  - 200
              - eq:
                - on_graphql_error
                - true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/4987