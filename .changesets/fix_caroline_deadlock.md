### Fix potential telemetry deadlock ([PR #7142](https://github.com/apollographql/router/pull/7142))

The `tracing_subscriber` crate uses `RwLock`s to manage access to a `Span`'s `Extensions`. Deadlocks are possible when
multiple threads access this lock, including with reentrant locks:
```
// Thread 1              |  // Thread 2
let _rg1 = lock.read();  |
                         |  // will block
                         |  let _wg = lock.write();
// may deadlock          |
let _rg2 = lock.read();  |
```

This fix removes an opportunity for reentrant locking while extracting a Datadog identifier.

There is also a potential for deadlocks when the root and active spans' `Extensions` are acquired at the same time, if
multiple threads are attempting to access those `Extensions` but in a different order. This fix removes a few cases
where multiple spans' `Extensions` are acquired at the same time.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7142
