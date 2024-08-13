### Add `format` for trace ID propagation. ([PR #5803](https://github.com/apollographql/router/pull/5803))

The router now supports specifying the format of trace IDs that are propagated to subgraphs via headers.

You can configure the format with the `format` option:

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

Note that incoming requests must be some form of UUID, either with or without dashes.

To learn about supported formats, go to [`request` configuration reference](https://apollographql.com/docs/router/configuration/telemetry/exporters/tracing/overview#request-configuration-reference) docs. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5803
