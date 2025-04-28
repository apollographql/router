### Fix coprocessor metrics documentation ([PR #7359](https://github.com/apollographql/router/pull/7359))

The docs for standard metric instruments for [coprocessors](https://www.apollographql.com/docs/graphos/routing/observability/telemetry/instrumentation/standard-instruments#coprocessor) has been updated:

- Renamed `apollo.router.operations.coprocessor.total` to `apollo.router.operations.coprocessor`
- Removed the unsupported `coprocessor.succeeded` attribute

By [@shorgi](https://github.com/shorgi) in https://github.com/apollographql/router/pull/7359