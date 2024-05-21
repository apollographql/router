### Add support for studio trace id in selectors and document context entry ([Issue #3803](https://github.com/apollographql/router/issues/3803)), ([Issue #5172](https://github.com/apollographql/router/issues/5172))

Add support for a new trace ID selector kind, the `apollo` trace ID, which represents the trace ID on [Apollo GraphOS Studio](https://studio.apollographql.com/). 

An example configuration using `trace_id: apollo`:

```yaml
telemetry:
  instrumentation:
    spans:
      router:
        "studio.trace.id":
            trace_id:: apollo
```

Add documentation for available rhai constants.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5189
