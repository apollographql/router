### Add `request_duration` router selector ([PR #9187](https://github.com/apollographql/router/pull/9187))

Adds a new `request_duration` selector for the router service that returns the total elapsed time from when the router received the request. The unit is configurable:

- `seconds` (float)
- `milliseconds` (integer)
- `nanoseconds` (integer)

The selector can be used as a custom instrument attribute or combined with conditions to filter based on request duration. For example, to count requests that complete in under 10 seconds:

```yaml
telemetry:
  instrumentation:
    instruments:
      router:
        my.short.requests:
          type: counter
          value: unit
          unit: reqs
          description: "Requests completing in under 10 seconds"
          condition:
            lt:
              - request_duration: seconds
              - 10
```

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9187
