### Rename the telemetry selector to get studio operation id ([PR #5337](https://github.com/apollographql/router/pull/5337))

We introduced a new `trace_id` selector format in `1.48.0` which was misnamed. It's not a trace id, it's the Apollo Studio Operation ID. We've fixed this naming problem in this release.

If you want to access this selector, here is an example:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        "studio.operation.id":
            studio_operation_id: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5337
