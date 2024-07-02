### Support conditions on standard telemetry events ([Issue #5475](https://github.com/apollographql/router/issues/5475))

Enables setting [conditions](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/conditions) on [standard events](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events/#standard-events).
For example:

```yaml title="router.yaml"
telemetry:
  instrumentation:
    events:
      router:
        request:
          level: info
          condition: # Only log the router request if you sent `x-log-request` with the value `enabled`
            eq:
            - request_header: x-log-request
            - "enabled"
        response: off
        error: error
        # ...
```

Not supported for [batched requests](https://www.apollographql.com/docs/router/executing-operations/query-batching/).
By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5476