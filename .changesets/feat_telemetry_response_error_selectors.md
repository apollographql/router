### Add router telemetry selectors to count and extract fields from GraphQL response errors ([PR #9448](https://github.com/apollographql/router/pull/9448))

The telemetry `RouterSelector` surface gains two optional selectors for working with the GraphQL `errors` list on the router response:

- **`response_errors_count`** — evaluates a JSONPath against the `errors` payload and exposes the match count as an integer OpenTelemetry value. Use a path like `$[*]` to count every error, or a filter expression to count only errors that match specific extension codes, messages, or other fields.
- **`response_errors_field`** — runs a JSONPath per error object and collects the matched values into an OpenTelemetry string array attribute, so you can attach structured error detail (for example `$.message` or `$.extensions.code`) to custom metrics or log pipelines.

These selectors follow the same response-body wiring as the existing `response_errors` selector, so they are available once the serialized router response body is available for inspection.

By [@smyrick](https://github.com/smyrick) and [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9448
