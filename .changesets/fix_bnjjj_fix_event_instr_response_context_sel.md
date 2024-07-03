### fix(telemetry): add response_context handling in event selector when using an event_* instrument ([PR #5565](https://github.com/apollographql/router/pull/5565))

This will fix cases when you want to create a custom instruments having a value set to `event_*` with a condition executed on event and using the `response_context` selector in attributes.

Example:

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