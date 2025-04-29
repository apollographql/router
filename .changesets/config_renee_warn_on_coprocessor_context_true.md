### Log warnings for deprecated coprocessor context configuration ([PR #7349](https://github.com/apollographql/router/pull/7349))

`context: true` is an alias for `context: deprecated` but should not be used. The router now logs a runtime warning on startup if you do use it.

Instead of:

```yaml
coprocessor:
  supergraph:
    request:
      context: true # ❌
```

Explicitly use `deprecated` or `all`:

```yaml
coprocessor:
  supergraph:
    request:
      context: deprecated # ✅
```

See [the 2.x upgrade guide](https://www.apollographql.com/docs/graphos/routing/upgrade/from-router-v1#context-keys-for-coprocessors) for more detailed upgrade steps.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/7349