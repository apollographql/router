### Add the ability to override span name with otel.name attribute ([Issue #5261](https://github.com/apollographql/router/issues/5261))

It gives you the ability to override the span name by using custom telemetry with any [selectors](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) you want just by setting the `otel.name` attribute.

Example:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        otel.name:
           static: router # Override the span name to router 
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5365