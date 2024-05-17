### Do not create custom events in telemetry if level is off ([PR #5165](https://github.com/apollographql/router/pull/5165))

Don't create custom events and attributes if you set the level to `off` 

example of configuration:

```yaml
telemetry:
  instrumentation:
    events:
      router:
        # Standard events
        request: info
        response: info
        error: info

        # Custom events
        my.disabled_request_event:
          message: "my event message"
          level: off # Disabled because we set the level to off
          on: request
          attributes:
            http.request.body.size: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5165