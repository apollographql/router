### Fix docs for 'exists' condition ([PR #5446](https://github.com/apollographql/router/pull/5446))

The example given for the condition `exists` in docs was wrong, it doesn't take an array but just a single selector instead.

Example of a good configuration

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