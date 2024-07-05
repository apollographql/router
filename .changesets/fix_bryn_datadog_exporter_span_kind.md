### Datadog `span.kind` now populated ([PR #5609](https://github.com/apollographql/router/pull/5609))

Datadog traces use `span.kind` to differentiate between different types of spans. 
This change ensures that the `span.kind` is correctly populated using the Open Telemetry span kind which has a 1-2-1 mapping to thouse set out in [dd-trace](https://github.com/DataDog/dd-trace-go/blob/main/ddtrace/ext/span_kind.go).

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5609
