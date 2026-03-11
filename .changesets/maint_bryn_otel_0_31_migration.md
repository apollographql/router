### Update to OpenTelemetry 0.31.0 ([PR #8922](https://github.com/apollographql/router/pull/8922))

The Router now uses v0.31.0 of the OpenTelemetry Rust libraries. This update includes many bug fixes and performance improvements from upstream. 

The Router does not guarantee the stability of downstream pre 1.0 APIs so users that directly interact with OpenTelemetry must update their code accordingly.

As part of this upgrade Zipkin Native exporter has been deprecated as this is being [deprecated upstream](https://opentelemetry.io/blog/2025/deprecating-zipkin-exporters/). Users should switch to OTLP exporter, which Zipkin now supports natively. 

Zipkin Native exporter also no longer supports setting service name, users that need this should switch to OTLP exporter. 


By [@BrynCooke](https://github.com/BrynCooke) [@goto-bus-stop](https://github.com/goto-bus-stop) [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8922
