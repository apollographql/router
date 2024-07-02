### Fix docs for 'exists' condition ([PR #5446](https://github.com/apollographql/router/pull/5446))

### Fix docs for 'exists' condition ([PR #5446](https://github.com/apollographql/router/pull/5446))

Fixes [documentation example](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/conditions/#exists) for the `exists` condition.
The condition expects a single selector instead of an array.

For example:

```yaml
telemetry:
  instrumentation:
    instruments:
      router:
        my.instrument:
          value: duration
          type: counter
          unit: s
          description: "my description"
          # ...
          # This instrument will only be mutated if the condition evaluates to true
          condition:
            exists:
              request_header: x-req-header
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5446