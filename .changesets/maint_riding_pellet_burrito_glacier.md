### Use a thread to clean up trace_providers rather than tokio blocking_task ([Issue #2668](https://github.com/apollographql/router/issues/2668))

Otel shutdown may sometime hang due to `Telemetry::Drop` using a `tokio::spawn_blocking` to flush the `trace_provider`.

Tokio doesn't finish executing tasks before termination https://github.com/tokio-rs/tokio/issues/1156.
This means that if the runtime is shutdown there is potentially a race where `trace_provider` may not be flushed.
By using a thread it doesn't matter if the tokio runtime is shut down.
This is likely to happen in tests due to the tokio runtime being destroyed when the test method exits.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2757
