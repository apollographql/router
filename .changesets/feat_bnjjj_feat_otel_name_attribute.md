### Override tracing span names using custom span selectors ([Issue #5261](https://github.com/apollographql/router/issues/5261))

Adds the ability to override span names by setting the `otel.name` attribute on any custom telemetry [selectors](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) .

This example changes the span name to `router`:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        otel.name:
           static: router # Override the span name to router 
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5365