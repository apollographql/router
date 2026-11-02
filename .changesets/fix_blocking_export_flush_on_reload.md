### Fix router reloads intermittently hanging due to blocking metrics/tracing export flushes ([PR #9787](https://github.com/apollographql/router/pull/9787))

Router config reloads could intermittently hang indefinitely. The metrics and tracing export pipelines (OTLP, Datadog, and Apollo Studio usage reporting) ran their periodic flush and shutdown work on the shared async worker pool, but that work does a real, synchronous blocking wait internally. Under the wrong timing, this could starve other work on the same thread — including unrelated connections' I/O readiness — long enough to hang the reload.

This is now fixed: that work runs on a dedicated blocking thread pool instead, so it can no longer starve the router's other async work.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9787
