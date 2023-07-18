### Add option to disable reuse of query fragments  ([Issue #3452](https://github.com/apollographql/router/issues/3452))

A new option has been added to the Router to allow disabling of the reuse of query fragments. This is useful for debugging purposes.
```yaml
supergraph:
  experimental_reuse_query_fragments: false
```

The default value depends on the version of federation.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3453
****
