### Rename trace telemetry selector ([PR #5337](https://github.com/apollographql/router/pull/5337))

[v1.48.0](https://github.com/apollographql/router/releases/tag/v1.48.0) introduced the `apollo` `trace_id` selector. `trace_id` is a misnomer for this metric, since the selector actually represents a GraphOS Studio operation ID. To access this selector, use `studio_operation_id`:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        "studio.operation.id":
            studio_operation_id: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5337