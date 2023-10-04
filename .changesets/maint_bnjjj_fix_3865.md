### Fix type handling for telemetry metric counter ([Issue #3865](https://github.com/apollographql/router/issues/3865))

Previously, the assignment of some telemetry metric counters may not have succeeded because the assignment type wasn't accounted for. For example, the following panicked in debug mode because `1` wasn't `1u64`:

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

This issue has been fixed by adding more supported types for metric counters.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3868
