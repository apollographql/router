### Fix Linux flake in file_upload::body_limits::rejects_oversized_operations_field

The `chunk_size_1_None` variant of `file_upload::body_limits::rejects_oversized_operations_field` flaked on Linux CI by panicking at the harness's 10 s `assert_shutdown` deadline with "unable to shutdown router".

The test built a single-shot streaming body and posted it through the default `reqwest::Client`, whose connection pool keeps the inbound TCP connection idle after the response. When the body arrives in one frame, the router fully drains it before multer trips the operations-field `SizeLimit` and returns 413, so the connection is pool-eligible from hyper's perspective. After the test calls `router.graceful_shutdown()`, the per-connection task in `handle_connection!` (`src/axum_factory/listeners.rs`) waits the full `connection_shutdown_timeout` (5 s default injected by the harness) for the idle client connection to close. On a loaded 2xlarge Linux runner that 5 s plus CI scheduling slack pushes total shutdown past the 10 s budget. The 100-byte chunked sibling variants escape because the body is aborted mid-upload, forcing the connection closed immediately.

The fix builds the request with `reqwest::Client::builder().pool_max_idle_per_host(0)`, matching the existing `no_keepalive_reqwest_client` pattern already used in `tests/integration/subgraph_response.rs` and `tests/integration/coprocessor.rs` for the same race. The test now closes its TCP connection as soon as the response is consumed, so the router exits within its normal shutdown window.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
