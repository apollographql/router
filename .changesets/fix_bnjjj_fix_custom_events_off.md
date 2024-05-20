### Custom telemetry events not created when logging is disabled ([PR #5165](https://github.com/apollographql/router/pull/5165))

The router has been fixed to not create custom telemetry events when the log level is set to `off`. 

An example configuration with `level` set to `off` for a custom event:

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