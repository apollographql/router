### Add a metric tracking authorization usage ([PR #3660](https://github.com/apollographql/router/pull/3660))

The new metric is a counter called `apollo.router.operations.authorization` and contains the following boolean attributes:
- filtered: some fields were filtered from the query
- requires_scopes: the query uses fields or types tagged with the `@requiresScopes` directive
- authenticated: the query uses fields or types tagged with the `@authenticated` directive

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3660