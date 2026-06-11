### Make `max_recursive_selections` configurable ([PR #9445](https://github.com/apollographql/router/pull/9445))

The router protects against deeply recursive or explosively large operations by counting the total number of selections encountered when recursively expanding fragment spreads. Previously this limit was hardcoded at 10,000,000. It can now be tuned via `limits.router.max_recursive_selections`:

```yaml
limits:
  router:
    max_recursive_selections: 10000000  # default
```

Reducing this value further restricts the complexity of operations the router will accept. The existing escape hatch (`APOLLO_ROUTER_DISABLE_SECURITY_RECURSIVE_SELECTIONS_CHECK`) still applies when the limit is exceeded.

Previously, setting `limits.router.warn_only` would not affect the max recursive selections check, this has now been changed to only emit a warning log if `warn_only` is set to true.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9445
