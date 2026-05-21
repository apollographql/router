### Fix Linux flake in response_cache::integration_test_basic (Redis readiness)

`integration::response_cache::integration_test_basic` was flaking on Linux CI with `Redis error … kind: Timeout` during the second `TestHarness` request. The router's response cache uses fred's default `default_command_timeout` of 500ms; under CI load the second harness's freshly-built fred pool was being asked to issue its first per-client lookup before Redis (or the host) had stabilised after teardown of the first harness's pool, exceeding the 500ms budget.

This is a test-only change. Before each `TestHarness::builder()` invocation in this test, we now prove Redis can complete a full PING round-trip from a brand-new fred client within a tight per-attempt budget, retrying against a deadline. If Redis can serve a cold-start command quickly, the router pool's first command will not race the 500ms timeout. No sleeps, no widened timeouts, no retries added to nextest, no `#[ignore]`.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
