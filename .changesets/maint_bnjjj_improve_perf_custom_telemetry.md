### Improve performance by optimizing telemetry meter and instrument creation ([PR #5629](https://github.com/apollographql/router/pull/5629))

The router's performance has been improved by removing telemetry creation out of the critical path, from being created in every service to being created when starting the telemetry plugin.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5629