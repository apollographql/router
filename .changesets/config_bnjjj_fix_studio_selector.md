### Rename the telemetry selector to get studio operation id ([PR #5337](https://github.com/apollographql/router/pull/5337))

We introduced a new `trace_id` selector format in `1.48.0` which has been misnamed because it's not a trace id but the Apollo Studio Operation ID. If you want to access to this selector, here is an example:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        "studio.operation.id":
            studio_operation_id: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5337