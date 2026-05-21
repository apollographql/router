### Fix macOS flake in `file_upload::operation_body_timeout::times_out_when_body_is_slow` ([PR #9489](https://github.com/apollographql/router/pull/9489))

The `times_out_when_body_is_slow` integration test flaked on macOS CI runners with the panic `unable to shutdown router, this probably means a hang and should be investigated` originating from `assert_shutdown_with_deadline`. The router itself was healthy — the failure was in the test's teardown ordering, not in `operation_body_timeout`.

The test ships a request body wrapped in a stream that sleeps 5 seconds before yielding bytes, then asserts that the router's 1 second `operation_body_timeout` fires with a 504. After observing the 504, the test invoked `graceful_shutdown()` immediately, while the client-side body stream was still mid-`sleep(5s)`. That left an open TCP connection with a pending request body the client had not yet sent.

The integration-test harness injects a 5 second `connection_shutdown_timeout` to bound exactly this case (see `merge_overrides` in `tests/common.rs`). The flake arose because the body stream's 5 second sleep matches the harness's 5 second `connection_shutdown_timeout` exactly — under macOS scheduler pressure the two windows could line up such that the router's per-connection drain in `handle_connection!` did not finish in time for the surrounding 10 second `assert_shutdown` deadline.

The fix is in the test, not in production code: replace the unconditional `sleep(5s)` body stream with a cancellable variant, and after the response is received in `times_out_when_body_is_slow`, explicitly cancel the body stream and drop the `reqwest::Client` so the client-side connection is torn down before `graceful_shutdown()` issues SIGTERM. This removes the wall-clock race entirely — the router's drain path now sees a closed connection rather than racing the body's pacing timer.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9489
