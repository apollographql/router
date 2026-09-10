### Fix flaky `entity_cache_basic` integration test ([Issue #9729](https://github.com/apollographql/router/issues/9729))

The test's short cache TTLs (2s/10s) could expire mid-test under CI load, racing against the wall-clock cost of building three sequential `TestHarness` instances and causing the invalidation-count assertion to see an already-expired (rather than freshly-invalidated) entity. TTLs are widened to 60s so they're no longer load-bearing for timing. (A separate, already-fixed failure mode in this same test — non-deterministic Redis key lookup — was resolved in #9666.) No production code changed.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9732
