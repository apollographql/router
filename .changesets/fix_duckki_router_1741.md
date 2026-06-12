### fix(format_response): Report missing fields as coercion errors and suppress redundant errors ([PR #9549](https://github.com/apollographql/router/pull/9549))

When a requested field is missing from the merged subgraph response, emit a
`RESPONSE_VALIDATION_FAILED` error in `response.errors` — turned on by
`enable_result_coercion_errors`. Previously, missing fields were only reported in
`extensions.valueCompletion` (and only for non-nullable fields), not in `response.errors`.

Additionally, redundant coercion and `valueCompletion` errors along null-bubble paths are now
suppressed. Previously, a single bad value inside nested non-null types could produce multiple
duplicate entries — one per non-null wrapper in the bubble chain. Now each coercion failure produces
exactly one originating error in `response.errors` and one `valueCompletion` entry at the source,
with no nesting-level duplicates.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9549
