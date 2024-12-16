### Ability to skip persisted query list safelisting enforcement via plugin ([PR #6403](https://github.com/apollographql/router/pull/6403))

If safelisting is enabled, a `router_service` plugin can skip enforcement of the safelist (including the `require_id` check) by adding the key `apollo_persisted_queries::safelist::skip_enforcement` with value `true` to the request context.

> Note: this doesn't affect the logging of unknown operations by the `persisted_queries.log_unknown` option.

In cases where an operation would have been denied but is allowed due to the context key existing, the attribute `persisted_queries.safelist.enforcement_skipped` is set on the `apollo.router.operations.persisted_queries` metric with value `true`.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/6403