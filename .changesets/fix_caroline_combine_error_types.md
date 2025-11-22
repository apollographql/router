### unify timeout codes in response caching metrics ([PR #8515](https://github.com/apollographql/router/pull/8515))

Unify the 'timeout' code used as a parameter in `apollo.router.operations.response_cache.*.error` metrics.

Tokio- and Redis-based timeouts should be treated as the same thing for the purpose of monitoring, but they were
inadvertently given different code values.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8515
