### Add `format` for trace ID propagation. ([PR #5803](https://github.com/apollographql/router/pull/5803))

When propagating trace ID to subgraph via header is is now possible to specify the format.

```yaml
telemetry:
  exporters:
    tracing: 
      propagation: 
        request: 
          header_name: "my_header"
          # Must be in UUID form, with or without dashes
          format: uuid
```

Note that incoming requests must some form of UUID either with or without dashes.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5803
