### Add path to SupergraphRequest ([Issue #4020](https://github.com/apollographql/router/issues/4020))

Coprocessor now supports `path` on `SupergraphRequest`. It can be enabled via router.yaml:

```yaml
coprocessor:
  supergraph:
    request:
      path: true
```

See the [coprocessor documentation](https://www.apollographql.com/docs/router/customizations/coprocessor/) for more details.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4021
