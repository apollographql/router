### Improve support of conditions at the request level, especially for events ([Issue #5702](https://github.com/apollographql/router/issues/5702))

`exists` condition is now properly handled with events, this configuration will now work:

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