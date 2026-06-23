### Fix macOS flake in `samples::/core/query2` (accounts subgraph ECONNRESET)

`samples::/core/query2` flaked on CircleCI's macOS executor when the public Apollo demo subgraph `https://accounts.demo.starstuff.dev/` reset the TCP connection mid-request. The router surfaced it as `SubrequestHttpError { service: "accounts", reason: "Connection reset by peer (os error 54)" }`, and the assertion diff between the canned `{"me":{"name":"Ada Lovelace"}}` and the resulting error payload failed the test. See CircleCI job 376290 (macOS, `prep-2.14.1`, 2026-05-20).

**Root cause**

`tests/samples/core/query2/plan.json` declared `"subgraphs": {}`. The samples test driver (`apollo-router/tests/samples_tests.rs::load_subgraph_mocks`) only registers a wiremock subgraph override per entry in that map, so with an empty map the router fell through to the hardcoded supergraph URL `https://accounts.demo.starstuff.dev/` for the `me { name }` query. The expected response `Ada Lovelace` is in fact the live response from that public demo subgraph, so the test was relying on real public-internet egress for its happy path. That made every `ECONNRESET` from the demo host an indistinguishable flake. Same shape as ROUTER-1823 in `apollo_reports`.

**What changed**

Added a single `accounts` mock entry to `tests/samples/core/query2/plan.json` that returns the same `{"me":{"name":"Ada Lovelace"}}` body the test already asserts on. This is the same pattern the sibling `samples::/core/query1` already uses for its `accounts` mock. With the mock present, `load_subgraph_mocks` inserts an `override_subgraph_url` entry that points `accounts` at the local wiremock server, so the test no longer egresses to `*.demo.starstuff.dev`.

Scope is intentionally narrow: only `core/query2` had the empty-subgraphs leak. All other samples (`core/query1`, `core/defer`, `basic/interface-object`, the `enterprise/*` suites) already provide the necessary subgraph mocks, so no framework-level change is needed.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9497
