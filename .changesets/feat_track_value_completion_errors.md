### Metrics for value completion errors ([PR #6905](https://github.com/apollographql/router/pull/6905))

When Router encounters a value completion error, it is not included in the GraphQL errors array, making it harder to observe. To surface this issue in a more obvious way, Router now counts value completion error metrics via the metric instruments `apollo.router.graphql.error` and `apollo.router.operations.error`, distinguishable via the `code` attribute with value `RESPONSE_VALIDATION_FAILED`.

By [@timbotnik](https://github.com/timbotnik) in https://github.com/apollographql/router/pull/6905
