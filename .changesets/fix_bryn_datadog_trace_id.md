### Attach `dd.trace_id` to json formatted log messages ([PR #4764](https://github.com/apollographql/router/pull/4764))

To enable correlation between DataDog tracing and logs, `dd.trace_id` must appear as a span attribute and also on the root of each `json` formatted log message.
If users configure their router.yaml to include `dd.trace_id`:

```yaml title="router.yaml"
telemetry:
  instrumentation:
    spans:
      mode: spec_compliant
      router:
        attributes:
          dd.trace_id: true
```
Then the `dd.trace_id` attribute will automatically be extracted from the root span and attached to the logs.


By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4764
