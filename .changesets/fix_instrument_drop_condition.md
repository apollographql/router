### Instrument incremented on aborted request when condition is not fulfilled ([PR #5215](https://github.com/apollographql/router/pull/5215))

Previously when a telemetry instrument was dropped it would be incremented even if the associated condition was not fulfilled. For instance:

```yaml
telemetry:
  instrumentation:
    instruments:
      router:
        http.server.active_requests: false
        http.server.request.duration: false
        "custom_counter":
          description: "count of requests"
          type: counter
          unit: "unit"
          value: unit
          # This instrument should not be triggered as the condition is never true
          condition:
            eq:
              - response_header: "never-received"
              - static: "true"
```

In the case where a request was started, but the client aborted the request before the response was sent, the `response_header` would never be set to `"never-received"`,
and the instrument would not be triggered. However, the instrument would still be incremented.

Conditions are now checked for aborted requests, and the instrument is only incremented if the condition is fulfilled.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5215
