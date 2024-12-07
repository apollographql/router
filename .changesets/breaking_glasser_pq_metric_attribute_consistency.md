### More consistent attributes on `apollo.router.operations.persisted_queries` metric ([PR #6403](https://github.com/apollographql/router/pull/6403))

Version 1.28.1 added several *unstable* metrics, including `apollo.router.operations.persisted_queries`.

When an operation is rejected, Router includes a `persisted_queries.safelist.rejected.unknown` attribute on the metric. Previously, this attribute had the value `true` if the operation is logged (via `log_unknown`), and `false` if the operation is not logged. (The attribute is not included at all if the operation is not rejected.) This appears to have been a mistake, as you can also tell whether it is logged via the `persisted_queries.logged` attribute.

Router now only sets this attribute to true, and never to false. This may be a breaking change for your use of metrics; note that these metrics should be treated as unstable and may change in the future.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/6403
