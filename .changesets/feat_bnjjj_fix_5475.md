### Telemetry: add support of condition on standard events ([Issue #5475](https://github.com/apollographql/router/issues/5475))

Now, you can also enable these standard events based on conditions (not supported on batched requests).

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

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5476