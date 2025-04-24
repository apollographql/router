### telemetry: correctly apply conditions on events ([PR #7325](https://github.com/apollographql/router/pull/7325))

Fixed a issue where conditional telemetry events weren't being properly evaluated.
This affected both standard events (`response`, `error`) and custom telemetry events.

For example in config like this:
```yaml
telemetry:
  instrumentation:
    events:
      supergraph:
        request:
          level: info
          condition:
            eq:
            - request_header: apollo-router-log-request
            - testing
        response:
          level: info
          condition:
            eq:
            - request_header: apollo-router-log-request
            - testing
```

The Router would emit the `request` event when the header matched, but never emit the `response` event - even with the same matching header.

This fix ensures that all event conditions are properly evaluated, restoring expected telemetry behavior and making conditional logging work correctly throughout the entire request lifecycle.

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/7325