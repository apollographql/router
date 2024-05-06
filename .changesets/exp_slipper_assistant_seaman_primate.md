### Experimental type conditioned fetching ([PR #4748](https://github.com/apollographql/router/pull/4748))

This release introduces an experimental configuration to enable type-conditioned fetching.

Previously, when querying a field that was in a path of two or more unions, the query planner wasn't able to handle different selections and would aggressively collapse selections in fetches. This resulted in incorrect plans.

Enabling the `experimental_type_conditioned_fetching` option can fix this issue by configuring the query planner to fetch with type conditions.


```yaml
experimental_type_conditioned_fetching: true # false by default
```

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/4748