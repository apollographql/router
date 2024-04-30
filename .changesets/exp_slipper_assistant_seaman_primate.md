### Type conditioned fetching ([PR #4748](https://github.com/apollographql/router/pull/4748))

Type conditioned fetching

When querying a field that is in a path of 2 or more unions, the query planner was not able to handle different selections and would aggressively collapse selections in fetches yielding an incorrect plan.

This change introduces an experimental configuration option to enable type conditioned fetching:

```yaml
experimental_type_conditioned_fetching: true # false by default
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/4748