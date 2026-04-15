### Add `expand_json_string_values` option to JSON log formatter ([PR #9156](https://github.com/apollographql/router/pull/9156))

When `expand_json_string_values: true` is set on a stdout or file JSON log formatter, string attribute values that contain valid JSON objects or arrays are emitted as native JSON rather than quoted strings. This allows log aggregators like Splunk to index sub-fields such as `errors{}.extensions.code`.

This is useful when telemetry selectors like `response_errors: "$[*]"` produce structured data: OpenTelemetry's attribute model serializes objects to JSON strings, but log formatters can now expand those strings back to native JSON at emit time. OTLP exporters are unaffected.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9156
