### Fix `exists` condition for custom telemetry events ([Issue #5702](https://github.com/apollographql/router/issues/5702))

The router now properly handles the `exists` condition for events. The following configuration now works as intended:

```yaml
telemetry:
  instrumentation:
    events:
      supergraph:
        my.event:
          message: "Auditing Router Event"
          level: info
          on: request
          attributes:
            graphql.operation.name: true
          condition:
            exists:
              operation_name: string
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5759