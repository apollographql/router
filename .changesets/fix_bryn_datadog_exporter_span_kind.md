### Populate Datadog `span.kind` ([PR #5609](https://github.com/apollographql/router/pull/5609))

Because Datadog traces use `span.kind` to differentiate between different types of spans, the router now ensures that `span.kind` is correctly populated using the OpenTelemetry span kind, which has a 1-2-1 mapping to those set out in [dd-trace](https://github.com/DataDog/dd-trace-go/blob/main/ddtrace/ext/span_kind.go).

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5609
