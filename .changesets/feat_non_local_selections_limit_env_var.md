### Configure the non-local-selections limit with `APOLLO_ROUTER_SECURITY_NON_LOCAL_SELECTIONS_LIMIT` ([Issue #RH-1408](https://apollographql.atlassian.net/browse/RH-1408))

Set the `APOLLO_ROUTER_SECURITY_NON_LOCAL_SELECTIONS_LIMIT` environment variable to raise the query planner's non-local-selections limit for operations that legitimately exceed the default of `100000`:

```bash
export APOLLO_ROUTER_SECURITY_NON_LOCAL_SELECTIONS_LIMIT=250000
```

The router rejects operations whose estimate exceeds the limit with a 400 HTTP status code and a GraphQL error with `"extensions": {"code": "QUERY_PLAN_COMPLEXITY_EXCEEDED"}`, including in `warn_only` mode. If the variable is missing, not a positive integer, or `0`, the router keeps the default and logs a warning. `APOLLO_ROUTER_DISABLE_SECURITY_NON_LOCAL_SELECTIONS_CHECK=true` still disables the check entirely.

By [@bryncooke](https://github.com/bryncooke)
