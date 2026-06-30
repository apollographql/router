### Ensure errors-as-data extensions deep-merge with connector error defaults ([PR #9575](https://github.com/apollographql/router/pull/9575))

When a connector's `isSuccess` evaluates to false and the user has configured `errors.extensions`, the resulting top-level error now correctly deep-merges the user-supplied extensions into the default extensions object. Previously, a mapping like `errors.extensions: "http: { myField: ... }"` would wipe out the default `http: { status }` field; now both appear side-by-side, matching the [public docs](https://www.apollographql.com/docs/graphos/connectors/responses/error-handling) contract that defaults are retained alongside user fields.

This PR also adds `Connector::output_shape()` as foundation API for downstream validators (entity-key checker, type walker) to reason about both the success and error branches of an errors-as-data connector via `Shape::one([selection.shape(), errors_shape()], [])`. No existing validator behavior changes in this PR.

By [@briannafugate](https://github.com/briannafugate) in https://github.com/apollographql/router/pull/9575
