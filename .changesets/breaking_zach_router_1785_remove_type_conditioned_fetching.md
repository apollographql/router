### Remove the `experimental_type_conditioned_fetching` configuration flag

Type conditioned fetching is now always enabled and can no longer be turned off via
`experimental_type_conditioned_fetching`. Without it, the query planner could merge
type-conditioned aliased entity fetches into a single unfiltered `Flatten`, sending every aliased
field to every resolved entity regardless of its concrete type — a correctness bug, not just a
performance one.

Existing configurations that still set this key are migrated automatically at startup: the router
logs a warning and removes the key, so startup no longer fails on it.

```diff
-experimental_type_conditioned_fetching: true
```

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/####
