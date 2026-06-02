### Bound `dump_stack_traces()` with a timeout so a wedged child can't eat the panic output

`IntegrationTest::dump_stack_traces` in `apollo-router/tests/common.rs` (Linux-only) is the synchronous diagnostic invoked immediately before a panic in three deadline-expiry sites (`wait_for_log_message`, `assert_log_not_contains`, `assert_shutdown_with_deadline`). It calls `rstack::TraceOptions::trace(pid)`, which has no internal timeout and is backed by `PTRACE_ATTACH`. If the target router child is in `TASK_UNINTERRUPTIBLE` or has wedged signal handling, the attach blocks indefinitely, outliving the panic and letting nextest's slow-timeout kill the whole process without ever surfacing the deadline message.

Wrap the `rstack` call in `tokio::task::spawn_blocking` + `tokio::time::timeout(10s)`. The function becomes `async`; the three callers are already in `async fn` so the migration is mechanical. On timeout, a clear message is logged so the operator knows the diagnostic was skipped — not that nothing happened. Happy-path `rstack::trace` on a responsive process completes in ~100 ms, well under the 10 s ceiling.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
