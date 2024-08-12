### Fix trace propagation via header ([PR #5802](https://github.com/apollographql/router/pull/5802))

The router now correctly propagates trace IDs when using the `propagation.request.header_name` configuration option.

```yaml
  exporters:
    tracing:
      propagation:
        request:
          header_name: "id_from_header"
```

Previously, trace IDs weren't transferred to the root span of the request, causing spans to be incorrectly attributed to new traces.
 
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5802
