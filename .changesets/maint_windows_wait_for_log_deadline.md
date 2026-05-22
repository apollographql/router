### Give `wait_for_log_message` extra headroom on Windows to stop Windows-only integration test flakes

`IntegrationTest::wait_for_log_message` (used by `assert_started`, `assert_reloaded`, `assert_not_started`, and friends) had a fixed 30 s deadline. Windows CircleCI runners spawn subprocesses and dispatch filesystem-watch events noticeably slower than Unix, so reload-driven waits frequently ran right up against the ceiling — most visibly causing intermittent Windows failures of `integration::telemetry::metrics::test_prom_reset_on_reload` and `integration::rhai::all_rhai_callbacks_are_invoked` on `dev`. Bump the deadline to 60 s on Windows only; Unix runs are unchanged.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9477
