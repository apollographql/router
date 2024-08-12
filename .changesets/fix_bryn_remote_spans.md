### Fix trace propagation via header ([PR #5802](https://github.com/apollographql/router/pull/5802))

The router now correctly propagates trace IDs when using the `propagation.request.header_name` configuration option.

```yaml
  exporters:
    tracing:
      propagation:
        request:
          header_name: "id_from_header"
```

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5802
