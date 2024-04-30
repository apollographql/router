### Add conditions on custom attributes for spans and a new selector for graphql errors ([Issue #4336](https://github.com/apollographql/router/issues/4336))

Example of configuration if you want to conditionally add attributes on a span. It's also using the new `on_graphql_error` selector which is set to true if the response body contains graphql errors.

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