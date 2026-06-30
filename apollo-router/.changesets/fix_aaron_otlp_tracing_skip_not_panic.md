### Fix `otlp::tracing` test family panic when GraphOS credentials are absent (ROUTER-1854)

10 tests in `tests/integration/telemetry/otlp/tracing.rs` used `panic!` instead of
`return Ok(())` when `graph_os_enabled()` returned false. `graph_os_enabled()` checks
for `TEST_APOLLO_KEY` / `TEST_APOLLO_GRAPH_REF`, which are only present in dev-branch
CI — not in PR-branch CI. The panic caused the entire `integration::telemetry::otlp::tracing::*`
family to fail deterministically on any PR that didn't have those env vars, even when
the PR's diff was completely unrelated to OTLP tracing.

Five other tests in the same file already used `return Ok(())` correctly; this aligns
the remaining ten.
