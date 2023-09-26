### fix(telemetry): support more types for metric counters ([Issue #3865](https://github.com/apollographql/router/issues/3865))

Add more supported types for metric counters in `MetricsLayer`.

Now it's not mandatory and won't panic in debug mode if you don't specify `1u64` in this example:

```rust
tracing::info!(
    monotonic_counter
        .apollo
        .router
        .operations
        .authentication
        .jwt = 1,
    authentication.jwt.failed = true
)
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3868
