### fix(QP-execution): Surface missing fields in `response.errors` ([PR #9549](https://github.com/apollographql/router/pull/9549))

When a requested field is missing from the merged subgraph response, emit a
`RESPONSE_VALIDATION_FAILED` error in `response.errors` — turned on by
`enable_result_coercion_errors`. Previously, non-nullable missing fields were only reported in
`extensions.valueCompletion` (not `response.errors`), while other coercion errors like null value
for non-nullable field were emitted to **both** `valueCompletion` and `response.errors`. This fix
updates Router to report all missing fields to `response.errors` if `enable_result_coercion_errors`
is on. `valueCompletion` behavior is not changed.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9549
