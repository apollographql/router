### Update to OpenTelemetry 0.31.0 ([PR #8922](https://github.com/apollographql/router/pull/8922))

The router now uses v0.31.0 of the OpenTelemetry Rust libraries. This update includes many bug fixes and performance improvements from upstream.

The router doesn't guarantee the stability of downstream pre-1.0 APIs, so users that directly interact with OpenTelemetry must update their code accordingly.

As part of this upgrade, Zipkin Native exporter is [deprecated upstream](https://opentelemetry.io/blog/2025/deprecating-zipkin-exporters/). Switch to the OTLP exporter, which Zipkin now supports natively. Note that Zipkin Native exporter no longer supports setting a service name — if you need this, switch to the OTLP exporter.

By [@BrynCooke](https://github.com/BrynCooke) [@goto-bus-stop](https://github.com/goto-bus-stop) [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8922
