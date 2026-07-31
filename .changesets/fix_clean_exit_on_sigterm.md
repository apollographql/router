### Fix router exiting with a non-zero status and losing buffered spans on SIGTERM ([PR #9910](https://github.com/apollographql/router/pull/9910))

On SIGTERM the router completed its graceful shutdown normally, then logged two panics on the way out and exited with status `1`:

```
A Tokio 1.x context was found, but it is being shutdown.
```

The tracing pipeline's batch span processors were never shut down. Their background loops only terminate on an explicit shutdown signal, so they were still running — and still polling Tokio timers — when the runtime was torn down at the end of `main`, which panicked. Orchestrators such as Kubernetes saw the resulting non-zero exit code as a failed container termination, and any spans still buffered in those processors at SIGTERM were silently discarded rather than flushed.

The router now shuts its tracer provider down explicitly before exiting, so buffered spans are flushed and SIGTERM produces a clean exit. Failures to shut the metrics pipeline down cleanly are also now reported instead of being silently swallowed.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9910
