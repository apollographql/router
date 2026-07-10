### Fix `OTEL_ORIGINAL_NAME` missing on renamed spans ([PR #9787](https://github.com/apollographql/router/pull/9787))

Spans that get renamed for export (for example, the Datadog exporter renaming the root `router` span to the GraphQL operation name for readability) were supposed to record their pre-rename name under an `otel.original_name` attribute, so tooling can still look the span up by its original, stable name. This stopped working after the OpenTelemetry 0.32 upgrade, silently dropping that attribute for every renamed span.

This is now fixed: renamed spans correctly carry their original name under `otel.original_name` again.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9787
