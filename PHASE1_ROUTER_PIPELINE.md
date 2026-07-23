# Phase 1 next-slice handoff — the source-aware router pipeline

> **STATUS — pipeline COMPLETE (all 5 slices landed, signed).** A live request
> now flows end-to-end through the source-aware path behind
> `experimental_connectors_source_aware` (default off). Commits on
> `benjamn/source-aware-phase0`:
> - **Slice 1** `6176c39e2` — `Connectors.by_coordinate` index (+ apply_config
>   dual-mutation drift guard).
> - **Slice 2** `a89dc86f6` — federation `SourceAwareQueryPlanner` entry point
>   (raw-graph plan + stamp); `from_query_graph`/`extract_subgraphs` stayed
>   `pub(crate)`.
> - **Slice 3** `14a80affd` — gated `spec/schema.rs` branch + federation
>   `unexpanded_connectors`; flag-off byte-identical.
> - **Slice 4a** `4f2702cb8` — source-aware planner wired into
>   `QueryPlannerService` (`into_parts` + `plan_inner` stamping).
> - **Slice 4b** `0c21a69eb` — dispatch by carried coordinate (`ConnectRequest`
>   + `ConnectorService`/factory `by_coordinate` + `resolve_connector`).
> - **Slice 5** `bb25e4417` — live end-to-end test: `{ users { id name } }`
>   through the real router matches the expansion path and hits `GET /users`.
>
> **Key simplification found:** B-2a needs no special graph — the only
> difference from `QueryPlanner::new` is the `validate_extracted_subgraphs`
> toggle, so source-aware planning = raw-SDL planner (validation off) + stamp.
> **Not yet done** (see "Open questions"): multi-connector merged fetch,
> type-level entity-resolver connectors, source-aware cost model, and B-2b
> (typed `SourceId` in the graph). The live entity path (`_entities` over a
> connector, `@requires`) works via the existing `entities_from_request` but
> only root-field is covered by a live test so far.

> **STATUS UPDATE — Phase-1 coverage (branch `benjamn/source-aware-phase1-coverage`,
> off `benjamn/source-aware-phase0` @ `254564009`).** Widening the steel thread
> from the single root-field class to the real query shapes. Commits (signed):
> - **Step 1** `47c9af376` — live **entity + `@requires`** end-to-end
>   (`source_aware_entity_plus_requires_end_to_end`, `{ users { id name d } }`):
>   root connector + graphql `_entities` (`c`) + `User.d` field-connector fetch,
>   matches expansion byte-for-byte.
> - **Step 2 repro** `6149eea61` — documented the multi-connector merged-fetch gap.
> - **Step 2A** `e1684d6e6` — **split the merged fetch per connector.**
>   `stamp_connector_coordinates` → `split_root_field_fetch`: when a `connectors`
>   fetch's top-level fields span >1 connector, replace the single `Fetch` with a
>   `Parallel` of per-connector fetches (filtered sub-operation via
>   `from_parsed(Valid::assume_valid)`, stamped coordinate, partitioned rewrites).
>   Reconstructs, in the plan, the fetch decomposition expansion got structurally.
>   `source_aware_multi_connector_merged_fetch_end_to_end` passes; full connectors
>   suite green (116 passed, 1 ignored), no flag-off regression.
>
> **Two framing insights (operator, this session):** (1) source-aware makes us
> responsible for reconstructing the **fetch decomposition/fan-out** expansion
> got for free from the O(#`@connect`) subgraph explosion — we reuse the existing
> Parallel/Sequence executor, we just re-establish the *boundaries* (that's 2A).
> (2) Per-connector **observability granularity** was another free side-effect of
> that explosion (subgraph-keyed metrics/spans); under one `connectors` subgraph
> it must be re-attached via the carried **coordinate** — a "source-aware
> observability parity" slice (survey pending: does connector telemetry key on
> coordinate/source or on the shared service name?).
>
> **Remaining, each with an executable repro or clear scope:**
> - **Step 3 — entity-resolver connectors** (`Query.user`, `entity: true`).
>   Ignored repro `source_aware_entity_resolver_connector_gap`: `username` →
>   `null` today because that class isn't stamped, so dispatch mis-resolves via
>   the many-to-one `by_service_name` fallback. Likely the same stamping
>   mechanism extended to the entity-resolver coordinate.
> - **Observability parity** (above).
> - **Step 4 — source-aware cost model**, with a demo requirement: prove to a
>   non-technical audience the plan gets *better* (fewer backend calls / fewer
>   round-trips) via a flag-off-vs-on A/B on an identical query. **Load-bearing
>   risk:** needs a query shape where source-aware cost yields a strictly cheaper
>   plan than expansion — a *divergence fixture* to find or construct (de-risk
>   early). Capture a countable request/round-trip metric in the harness from now
>   on so the before/after is a recorded diff.
> - **2A follow-ons:** entity-field merges (shared representations) and
>   variable-bearing merges — `split_root_field_fetch` deliberately leaves these
>   unsplit today.

*Self-contained brief for a fresh agent. The connector **dispatch** logic is
built and tested piecewise (the "(B) identity spine", below); what remains is
wiring a **source-aware router pipeline** so a live request actually flows
through it — raw-graph planning + coordinate-indexed connectors + the routing
call site. This doc maps the router service-construction seam and specs the
gated mode.*

Read first, in order:
1. `SOURCE_AWARE_DISTANCE.md` — the evidence-based distance assessment. Its
   "Phase 1 — the router fetch seam" section (Slices 1/2, 3a/3b, and the **(B)**
   track: B-1/B-2a/B-3) is what this builds on.
2. `PHASE1_FETCH_SEAM_HANDOFF.md` — the fetch-seam brief this continues.
3. `PHASE0_HANDOFF.md` — original Phase-0 grounding.
4. Rev-2 proposal at narrative `claude-h371fb7k:31` — plan of record.

## Context: branch, worktree, process

- **Branch:** `benjamn/source-aware-phase0`. **Worktree:**
  `/Users/ben/dev/router-source-aware-phase0`.
- **Process:** stay in this worktree; **no subagents**; commit each slice
  separately; **every commit GPG-signed** (never `--no-gpg-sign`); **never
  delete** (`cp`, not `mv`); no PR — this is a no-schedule feasibility spike,
  optimize for insight and honest findings, not ship-quality.

## What is already built (so you don't redo it)

The connector **identity + dispatch** work is done and tested; it is *carried*,
not recovered heuristically (the architectural principle: expansion's synthetic
subgraphs were an identity-carrying device; source-aware carries a lightweight
coordinate instead).

- **B-1 — identity channel.** `FetchNode.connector: Option<String>` (the
  connector coordinate) on both the federation `FetchNode`
  (`apollo-federation/src/query_plan/mod.rs`) and the router `FetchNode`
  (`apollo-router/src/query_planner/fetch.rs`), threaded through
  `apollo-router/src/query_planner/convert.rs`. `None` default, serde-skipped,
  not shown in Display ⇒ existing plans byte-identical.
- **B-2a — authoritative stamping.**
  `apollo-federation/src/query_plan/connector_stamp.rs` ::
  `stamp_connector_coordinates(&mut QueryPlan, &[Connector])` sets each connector
  fetch's coordinate by matching its target `(type, field)` to the ground-truth
  `Connector` set (root fetch → `Query.users`; entity fetch
  `_entities { … on User { d } }` → `User.d`). Determined once, at plan time,
  from `@connect` metadata. Tested over raw-graph steelthread plans.
- **B-3 — dispatch by carried coordinate.**
  `apollo-router/src/plugins/connectors/query_plans.rs` ::
  `resolve_connector(fetch_connector, service_name, by_coordinate,
  by_service_name)` prefers the carried coordinate, falls back to service name.
  `connectors_by_coordinate(&ConnectorsByServiceName)` builds the index. Tested;
  `source_aware_dispatch_by_carried_coordinate` (in `make_requests.rs`) stitches
  the whole vertical in the router: carried coordinate → resolve → real
  `make_requests` → actual `GET …/users`.
- **make_requests layer proven for both classes:** root-field
  (`source_aware_root_field_dispatch`) and entity (`source_aware_entity_dispatch`
  + `_end_to_end`) both produce real connector HTTP requests from a plain
  supergraph selection, no synthetic `_entities` operation authored by us. (Note:
  the raw-graph planner *does* emit standard `_entities` ops for entity fetches —
  see the `dump_raw_graph_entity_plan` receipt — so the existing
  `entities_from_request` also handles them; `entities_from_source_aware` (3a) is
  for a future planner that emits none.)

**The (B) spine, end to end (piecewise-proven):**
`B-2a stamp (fed) → B-1 carry → B-3 resolve-by-coordinate (router) →
make_requests → real request`. What is *not* yet done is making a **live
request** flow through it — that is this pipeline.

## The router service-construction map

One decisive fork, three downstream consumers.

### The fork: `Schema::parse` → `expand_connectors`

`apollo-router/src/spec/schema.rs:70` calls `expand_connectors(&raw_sdl.sdl, …)`.
On a connector supergraph it returns `ExpansionResult::Expanded { raw_sdl,
api_schema, connectors }`, where:
- `raw_sdl` is the **expanded** SDL (connectors rewritten into synthetic
  subgraphs) — **and this is the SDL the router parses and the planner plans
  over** (`schema.rs` stores it; the planner service builds from it).
- `connectors: Connectors` (federation
  `apollo_federation::connectors::expand::Connectors`) carries `by_service_name:
  IndexMap<Arc<str>, Connector>` (synthetic-subgraph-name → connector) plus
  `labels_by_service_name`.

So expansion does **two jobs here at once**: rewrite-for-planning *and* build the
connector index. Source-aware **splits them**: keep the original raw SDL for
planning (the spike proved planning works over it), and build a
**coordinate-indexed** connector set.

### Consumer 1 — the planner

The query planner (built in `apollo-router/src/query_planner/…`, e.g.
`query_planner_service.rs`) plans over the stored (currently expanded) SDL.
Source-aware: plan over the **raw** SDL via
`apollo_federation::…::QueryPlanner::from_query_graph` (inject a graph built by
`build_federated_query_graph`), then call `stamp_connector_coordinates` (B-2a) on
the resulting plan.

### Consumer 2 — the connector service factory

`apollo-router/src/services/supergraph/service.rs:573`:
```rust
Arc::new(ConnectorServiceFactory::new(
    schema.clone(),
    subgraph_schemas,
    subscription_plugin_conf,
    schema.connectors.as_ref().map(|c| c.by_service_name.clone()).unwrap_or_default(),
    …,
))
```
Source-aware: hand the factory a **by-coordinate** index (and/or keep
`by_service_name` for fallback).

### Consumer 3 — dispatch

`apollo-router/src/services/fetch_service.rs:143`
(`fetch_with_connector_service`) resolves
`schema.connectors.by_service_name.get(&fetch_node.service_name)` and builds the
`ConnectRequest` keyed by `service_name`. Source-aware: call `resolve_connector`
(B-3) with `fetch_node.connector` + the by-coordinate index. `ConnectorService::
call` (`connector_service.rs:131`) likewise keys on `request.service_name` — it
needs the coordinate too (thread `fetch_node.connector` onto the `ConnectRequest`,
built at `fetch_service.rs:151`).

## The gated source-aware mode — design

A config flag selects source-aware. At the `expand_connectors` fork
(`spec/schema.rs:70`):

1. **Schema build branch.** When source-aware is on and the supergraph has
   connectors: **skip expansion**. Keep the original raw SDL as the SDL to parse
   and plan over. Build connectors directly from the raw subgraphs
   (`Connector::from_schema`) and index them by coordinate.
2. **`Connectors` gains a `by_coordinate` index** (federation
   `expand::Connectors`) alongside `by_service_name`, so the router carries the
   coordinate-keyed set the same way it carries the service-name-keyed one.
3. **Planner** uses `from_query_graph` over the raw SDL + `stamp_connector_
   coordinates`. (Simplest wiring: a source-aware branch in the planner service's
   plan step.)
4. **Factory + dispatch** read the by-coordinate index via `resolve_connector`.

Default (flag off) path stays byte-identical: `expand_connectors` runs exactly
as today.

## Prerequisites (federation API surface)

The router cannot currently build a raw-graph stamped plan because these are
`pub(crate)` in `apollo-federation`:
- `QueryPlanner::from_query_graph` (`query_plan/query_planner.rs:298`)
- `extract_subgraphs_from_supergraph` (`supergraph/mod.rs:302`)

Expose the minimum needed (prefer a small, purpose-named `pub` entry point over
opening these broadly). `Connector::from_schema` and `ConnectId::coordinate` are
already `pub`. `stamp_connector_coordinates` is already `pub`.

## Suggested incremental slices (commit + test each, signed)

1. **`Connectors.by_coordinate`** — add the index to federation `expand::
   Connectors`; populate it wherever `by_service_name` is populated. Additive.
2. **Federation entry point** — a `pub` function that, given a raw supergraph
   SDL, returns a stamped raw-graph `QueryPlan` for an operation (wraps
   `build_federated_query_graph` + `from_query_graph` + `stamp_connector_
   coordinates`). Unit-test it end to end in federation.
3. **Router schema-build branch** — gated on a config flag: skip expansion, keep
   raw SDL, build `Connectors` with `by_coordinate`. Verify flag-off is
   byte-identical.
4. **Planner + factory + dispatch wiring** — plan via the source-aware entry
   point; hand the factory the by-coordinate index; thread `fetch_node.connector`
   into `ConnectRequest`; dispatch via `resolve_connector`.
5. **Live request test** — a router integration test issuing `{ users { id name } }`
   against a mocked connector HTTP endpoint, asserting the real response — the
   first *live* source-aware request.

## How to verify (reuse what exists)

- Correctness oracle `crate::correctness::check_plan`; the `plan-diff` CLI
  (`cli/src/plan_diff.rs`, `PlanMode::SourceAware` seam) for corpus parity.
- Federation diagnostics: `stamps_connector_coordinates_over_raw_graph_plans`,
  `dump_raw_graph_entity_plan`, `raw_vs_expanded_plan_diff`, `mirage_check_*`,
  `steel_thread_root_field_end_to_end`.
- Router: `source_aware_dispatch_by_carried_coordinate`,
  `resolve_connector_prefers_carried_coordinate`, `source_aware_*_dispatch*`.

## Open questions / risks

- **B-2a vs B-2b.** B-2a stamps coordinates on a plan produced by the
  collapsed-graph planner (proven, low risk). **B-2b** — grafting typed-`SourceId`
  source-entering edges into the live `QueryGraph` so identity flows through
  traversal — is the production-correct graph model but the highest-blast-radius,
  plan-reshaping change (resumes the connect-graph integration the earlier
  session was told to hold on, `claude-czk6ds56:44`). The pipeline can ship on
  B-2a; B-2b is a later refinement.
- **Multi-connector merged fetch.** A single `connectors` fetch can in principle
  merge fields from several connectors on one type (fetch-merge keys on subgraph
  name). B-2a leaves such a fetch unstamped (resolves to >1 connector); the
  planner-side fix is to key fetch-merge/cost on connector *source*, not subgraph
  name (predecessor's seq-84 point). Out of scope until it appears in a fixture.
- **Type-level entity-resolver connectors** (`@connect` on the type) — B-2a
  stamping handles field connectors; extend `collect_targets`/matching for type
  connectors.
- **Cost model.** With connectors collapsed into one subgraph, the planner's
  boundary-crossing cost no longer prices per-connector-call. Real source-aware
  cost must price per source-entering edge (predecessor's seq-84 point 1). Not
  needed for a working thread; needed for *good* plans.
