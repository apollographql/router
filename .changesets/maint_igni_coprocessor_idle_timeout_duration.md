### Coprocessor: Set a default pool idle timeout duration. ([PR #3434](https://github.com/apollographql/router/pull/3434))

Having a too high idle pool timeout durations can sometimes trigger situations in which an HTTP request cannot complete (see [this comment](https://github.com/hyperium/hyper/issues/2136#issuecomment-589488526) for more information).

This changeset sets a default timeout duration of 5 seconds, which we may make configurable eventually.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3434
