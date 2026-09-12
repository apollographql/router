### Add `limits.router.max_non_local_selections` to configure the non-local selections limit

Set `limits.router.max_non_local_selections` to allow operations that exceed the default query-planning estimate of `100000`:

```yaml title="router.yaml"
limits:
  router:
    max_non_local_selections: 250000
```

The router rejects operations whose estimate exceeds this limit, including in `warn_only` mode. `APOLLO_ROUTER_DISABLE_SECURITY_NON_LOCAL_SELECTIONS_CHECK=true` disables the check.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10211
