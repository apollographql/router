### Add `response_context` in event selector for `event_*` instruments ([PR #5565](https://github.com/apollographql/router/pull/5565))

The router now supports creating custom instruments with a value set to `event_*` and using both a condition executed on an event and the `response_context` selector in attributes. Previous releases didn't support the `response_context` selector in attributes.

An example configuration:

```yaml
telemetry:
  instrumentation:
    instruments:
      supergraph:
        sf.graphql_router.errors:
          value: event_unit
          type: counter
          unit: count
          description: "graphql errors handled by the apollo router"
          condition:
            eq:
            - true
            - on_graphql_error: true
          attributes:
            "operation":
              response_context: "operation_name" # This was not working before
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5565