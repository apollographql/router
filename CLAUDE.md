# Apollo Router — Engineering Guidelines

This file applies both when writing new code and when reviewing it: the "Required" rule below must be followed by anyone (or any AI tooling) authoring changes, and the "Known patterns to check" list is what to look for during code review — including whether the "Required" rule was actually followed.

## Required when writing code

### Usage metric for every new config option or feature

Any new config option or feature must ship with a usage metric, so adoption is measurable from day one. Knowing how many deployments actually use a feature is what makes decisions like deprecation, further investment, or support prioritization possible — without it, those calls get made blind.

- The metric must be **always-on**: emitted once per router startup/config-reload (an `ObservableGauge` populated from the parsed config), not a per-request counter that scales with traffic volume. A feature used by one router instance serving zero requests must still be visible.
- Follow the existing pattern in `populate_config_instruments` (`apollo-router/src/configuration/metrics.rs`), which registers instruments via the `populate_config_instrument!` macro. Name the metric `apollo.router.config.<feature>` and add it there — do not build a parallel mechanism.
- It's fine for the metric to just record "this is enabled" with no immediate consumer. The goal is a black-hole signal — cheap to emit, ignorable for now, queryable later if adoption becomes a question.

Example, following `apollo.router.config.authorization` in `apollo-router/src/configuration/metrics.rs`:

```rust
populate_config_instrument!(
    apollo.router.config.my_new_feature, // metric name
    "$.my_new_feature[?(@.enabled == true)]", // path to the feature in config
    opt.some_sub_option, // attribute name
    "$[?(@.some_sub_option == true)]" // path to the attribute, relative to the feature
);
```

This sets `apollo.router.config.my_new_feature` to `1` whenever `my_new_feature.enabled: true` is present in the router's YAML config (with an `opt_some_sub_option` attribute for the sub-option), and reports it on every metrics collection cycle regardless of request traffic.

## Known patterns to check during code review

The following patterns are known sources of bugs in this codebase. Check for all of them in addition to general correctness, performance, and security concerns.

**Missing usage metric for a new config option or feature.**
- When a diff adds a new config option or feature flag, check whether `populate_config_instruments` in `apollo-router/src/configuration/metrics.rs` was updated to register it.
- Flag config/feature additions with no corresponding `apollo.router.config.*` gauge — see the "Required" section above.

**Test asserts on the wrong side of the wire.**
- For each external call in the diff (HTTP, IPC, DB, etc.), ask: where's the metric/log/span that records it happened?
- Find the test exercising the new path. Does it assert on the internal observable, or only on the fake's receipt?
- If only the fake is checked, the recording path is untested.

**Eager observation of lazy work.**
- For every function returning a `Stream`, `Iterator`, `Future`, `impl FnOnce`, or value containing one: identify which work is eager vs. lazy.
- Flag synchronous checks on the function's effects (out-params, "ran?" booleans) that fire before the lazy work runs — they only see the eager half.
- For each side effect in the eager half (metric, log, audit), ask: is there a symmetric code path inside the lazy value where the same event can happen?

**Shape-isomorphic containers with different propagation semantics.**
- For every `.extensions().insert()` / `.context.insert()` / `.metadata.set()` in the diff, ask: what manipulates this container before the data is read?
- Search for `Clone for <enclosing-type>`, `from_parts`, `into_parts`, `serde` derives that don't include the extension.
- If data is read in a different module from where it's written, read the travel path between them — the diff doesn't show it.

**The mock bypasses the path under test.**
- When a test uses `Mock*Service` / `Mock*Repository` / `Fake*Client`, ask: what's the unmocked path through this code? Is it covered elsewhere?
- For PRs that introduce or relocate shared state: trace each read-site's call graph — is each read-site exercised by at least one test running against real code?

**Stale "for now" comments as unenforced invariants.**
- Flag "for now" / "until X" / "doesn't matter because Y" comments — ask what *enforces* this.
- When a diff adds usage of something a comment said "we don't use," the comment is now wrong.

**Fixed sleeps instead of condition-based waits.**
- Flag `tokio::time::sleep` / `std::thread::sleep` in test code where the next statement is an assertion.
- Flag "spawn + sleep N ms" patterns for server readiness.
- Flag comments that say "increase this time if the test fails."

**Hard-coded ports in tests.**
- Flag specific port numbers (`4000`, `4001`, `4005`, `8080`, etc.) in new test YAML fixtures.
- Flag `TcpListener::bind("127.0.0.1:<fixed>")` in test code.

**Global / shared mutable state without serialization.**
- Flag tests that call `Telemetry::activate()`, install a global tracing subscriber, or set `APOLLO_KEY` / `APOLLO_GRAPH_REF` without scoping/restoring.
- Ask: is this test in a serializing test-group in `.config/nextest.toml`?
- Flag test-local `static Mutex` inside `tests/*.rs` — it serializes nothing across integration binaries.

**Telemetry assertions on a single flush.**
- Flag telemetry tests that assert on `reports[0]` or `get_traces()[0]` without accumulating across multiple reports.
- Flag `get_traces` / `get_report` calls with hard-coded short retry windows.

**Non-deterministic collection ordering in assertions.**
- Flag snapshot tests or `assert_eq!` calls comparing serialized JSON objects or metric label maps without sorting.
- Flag `HashMap::iter()` results fed directly into an ordered assertion.

**Static Redis namespace in tests.**
- Flag any Redis config block in test YAML with a hard-coded `namespace:` key.
- If a PR adds a new Redis-backed feature, check that its config path is added to `insert_redis_namespace` in `merge_overrides`.

**`threads-required` used as a serialization tool.**
- When a diff adds `threads-required = N`, ask: is this test CPU-bound, or guarding against a shared global? If the latter, suggest a test-group instead.

**Nanosecond-timer races in tests.**
- Flag `Duration::from_nanos(N)` in test timeout calls.
- Flag any test that asserts on whether a timeout fires vs. the work completes.

**Live-network coupling in tests.**
- Flag `with_real_studio_creds()` / `with_subgraph_network_requests()` in new tests.
- Flag schema URLs pointing to `*.demo.starstuff.dev` or `apollo.dev` in test fixtures.

**Plugin order trap: service-replacing before observing.**
- When a test mixes `MockedSubgraphs` with any other `extra_plugin`, check ordering: `MockedSubgraphs` must be last.

**Windows file-watcher on identical content.**
- Flag new helpers that write config to disk and bypass `common.rs::update_config`.
- Flag tests that write the same config string twice and then call `assert_reloaded()`.

**`std::time::Instant` in tokio-pauseable scheduler code.**
- Flag `std::time::Instant::now()` inside any struct or function used with `tokio::time::DelayQueue`, `tokio::time::sleep_until`, or other tokio time primitives.

**Shutdown-drain race: keep-alive client holds the router open.**
- Flag tests that make HTTP/2 requests and then call `graceful_shutdown()` — ask whether the client has keep-alive disabled.
