### Add `subscription.enabled` field to enable subscription support ([Issue #3428](https://github.com/apollographql/router/issues/3428))

`enabled` is now required in `subscription` configuration. Example:

```yaml
subscription:
  enabled: true
  mode:
    passthrough:
      all:
        path: /ws
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3450