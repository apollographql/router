### Particular `supergraph` telemetry customizations using the `query` ([PR #6324](https://github.com/apollographql/router/pull/6324))

Telemetry customizations like those featured in the [request limits telemetry documentation](https://www.apollographql.com/docs/graphos/routing/security/request-limits#collecting-metrics) now work as intended when using the `query` selector on the `supergraph`.  In some cases, this was causing a `this is a bug and should not happen` error, but is now resolved.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6324