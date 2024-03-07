### Attach `dd.trace_id` to JSON formatted log messages ([PR #4764](https://github.com/apollographql/router/pull/4764))

To enable correlation between DataDog tracing and logs, `dd.trace_id` must appear as a span attribute on the root of each JSON formatted log message.
Once you configure the `dd.trace_id` attribute in router.yaml, it will automatically be extracted from the root span and attached to the logs:

```yaml title="router.yaml"
telemetry:
  instrumentation:
    spans:
      mode: spec_compliant
      router:
        attributes:
          dd.trace_id: true
```


By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4764
