### Unify timeout codes in response caching metrics ([PR #8515](https://github.com/apollographql/router/pull/8515))

Tokio- and Redis-based timeouts now use the same `timeout` code in `apollo.router.operations.response_cache.*.error` metrics. Previously, they were inadvertently given different code values.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8515
