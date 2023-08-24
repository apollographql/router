### add a metric tracking authorization usage ([PR #3660](https://github.com/apollographql/router/pull/3660))

The new metrics, for use in Router Analytics, is a counter called `apollo.router.operations.authorization`
and contains the following boolean attributes:
- filtered: some fields were filtered from the query
- authenticated: the query uses fields or types tagged with the `@authenticated` directive
- requires_scopes: the query uses fields or types tagged with the `@requiresScopes` directive

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3660