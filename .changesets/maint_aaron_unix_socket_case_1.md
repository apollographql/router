### Fix macOS flake in unix_tests::test_unix_socket_max_header_list_size::case_1 ([PR #9491](https://github.com/apollographql/router/pull/9491))

`integration::http_server::unix_tests::test_unix_socket_max_header_list_size::case_1_header_within_limits_of_config` had a residual flake on macOS arm64 CI even after the `drop(sender) + graceful_shutdown_with_deadline(20s)` pattern from PR #9418 was applied to its shared `#[rstest]` function body.

The companion `case_2_header_bigger_than_config` (server rejects with 431 before reading the body) was fully closed by that prior fix. `case_1` (server accepts the 10 MiB header and returns a successful GraphQL response) has one extra shoulder of the same drain race: the response body was never consumed before drop. In HTTP/2, dropping an unread `Incoming` body sends `RST_STREAM` on the response stream, which forces the server-side response-writer task through an error-path teardown instead of the END_STREAM happy path. On a busy macOS arm64 runner this extra cleanup — stacked on top of the 10 MiB request-header parse the server is still finishing — was enough to push the post-SIGTERM drain past `assert_shutdown`'s budget. `case_2` does not exhibit this shoulder because the 431 response carries no body to leave unread.

Fix: drain the response body to its natural END_STREAM with `body.collect().await` before dropping the sender. Applied unconditionally (it's a no-op on the 431 path's empty body) to keep the test linear and avoid a status-conditional split. The `drop(sender) + graceful_shutdown_with_deadline(20s)` pattern from #9418 stays in place — this is additive, not a replacement.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9491
