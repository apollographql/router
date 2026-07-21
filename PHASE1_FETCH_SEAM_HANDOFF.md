# Phase 1 next-slice handoff — the router fetch seam

*Self-contained brief for a fresh agent picking this up in a new context. The
planner-side feasibility spike is done and its conclusion is durable; this doc
specs the **next** slice — the router fetch executor — with the exact anchors
you need so you don't have to re-survey.*

Companion docs (read these first, in order):
1. `SOURCE_AWARE_DISTANCE.md` — the evidence-based distance assessment. **Start
   here.** Its "Corpus-wide plan parity" and "Mirage check" sections are the
   findings this handoff builds on.
2. `PHASE0_HANDOFF.md` — the original Phase-0 grounding (Spike A/B briefs, the
   connector-input classification table with file:line anchors).
3. The rev-2 proposal at narrative `claude-h371fb7k:31` (the plan of record for
   the whole effort; this handoff is one slice of its Phase 1).

## Context: branch, worktree, process

- **Branch:** `benjamn/source-aware-phase0`, tip `7672758c5`.
- **Worktree:** `/Users/ben/dev/router-source-aware-phase0`.
- **Base:** `14335e254` (`origin/dev`) + `benjamn/precise-connector-output-shapes`
  merged in (shape `0.8.0-preview.2` + `JSONSelection::shape_with_vars`), i.e.
  the merge at `3ef9cbb2e`.
- **Process constraints (carried from the whole effort):** stay in this
  worktree; **no subagents**; commit each slice separately; **every commit
  GPG-signed** — never pass `--no-gpg-sign` (pinentry works); **never delete
  anything** (`cp`, not `mv`; no `rm`/`git rm`/`git clean`). This is a
  no-schedule feasibility spike, **no PR** planned — optimize for insight and
  honest findings, not ship-quality.

## What is already proven (so you don't redo it)

The existing query planner **already produces correctness-validated plans for
connector supergraphs over the raw, non-expanded graph** — across the entire
expand fixture corpus (14 fixtures / 32 ops, all `Equivalent` vs expansion; see
`SOURCE_AWARE_DISTANCE.md`). Synthetic-subgraph expansion is an **execution-layer
device, not a planning necessity**: composition emits full join metadata
(`@join__type(key:)`, `@join__field(requires:)`) and the planner treats
`connectors` as an ordinary subgraph, ignoring `@connect`.

The catch, and the whole reason this slice exists: those correct plans emit
`Fetch(service: "connectors")` — **a subgraph fetch that no real service
backs**. Turning that into actual connector HTTP calls is the router fetch seam.
That is the bulk of the remaining Phase-1 distance, and it is what you are
building.

Reference implementation already in-tree: `steel_thread_root_field_end_to_end`
(`apollo-federation/src/query_plan/query_planner.rs:1645`) hand-wires the full
root-field thread (plan → `GET /users` → mapped response) in-crate. It proves
the pieces connect; your job is to make the plan→dispatch step *automatic* in
the router instead of hand-wired.

## The task: dispatch `Fetch(connectors)` as connector requests

**Goal of this slice:** given a source-aware query plan whose fetch nodes target
the `connectors` subgraph, execute them as real connector requests — without the
synthetic-subgraph schema that expansion normally provides.

### The seam and the exact anchors

- **Payload (federation side, already built):** `ConnectFetchDescriptor`
  (`apollo-federation/src/connectors/source_aware.rs:222`, built by
  `ConnectFetchDescriptor::build` at `:242`). It carries: `coordinate` (stable
  connector id for direct dispatch, replacing the synthetic service name),
  `entity_resolver`, `output_type`, `output_selection` (selection against the
  **supergraph** schema, *not* a synthetic-schema operation), `condition`
  (Spike-A parent-data `FieldSet`), and `inputs` (Spike-A classification).

- **Executor entry (router side):** `make_requests`
  (`apollo-router/src/plugins/connectors/make_requests.rs:25`). Today it takes a
  `Valid<ExecutableDocument>` **operation written against the synthetic subgraph
  schema** plus the `Connector`, and produces `Vec<ResponseKey>` →
  `Vec<Request>`. It branches on `connector.entity_resolver`:
  - `None` → `root_fields` (`make_requests.rs:143`)
  - `Explicit | TypeSingle` → `entities_from_request` (`:220`)
  - `Implicit` → `entities_with_fields_from_request` (`:304`)
  - `TypeBatch` → `batch_entities_from_request` (`:462`)
  then `request_params_to_requests` (`:57`) builds the HTTP requests via
  `runtime::http_json_transport::make_request`.

- **Dispatch keying (the thing to change):** connectors are resolved at
  execution time **by synthetic service name** —
  `ConnectorsByServiceName = Arc<IndexMap<Arc<str>, Connector>>`
  (`apollo-router/src/plugins/connectors/query_plans.rs:9`, stored/read via
  `store_connectors`/`get_connectors` at `:13`/`:20`; also used in
  `plugin.rs:158` and the `service_usage_set()` walk at `plugin.rs:164`). In
  source-aware there is **one** `connectors` subgraph, so the fetch node's
  `service_name` no longer identifies *which* connector — the descriptor's
  `coordinate` does. Dispatch must key on that instead.

### Why it's not just "call the existing function"

Two real forks, in rough order of difficulty:

1. **Root-field class — closest to working.** `root_fields` (`:143`) reads the
   operation field's sub-selection + arguments and applies `connector.selection`.
   The steel thread shows this already yields the right request from a
   supergraph-level operation. The gap is mostly **dispatch keying** (service
   name → `coordinate`) and confirming argument sourcing lines up. Start here.

2. **Entity class — the genuine rework.** `entities_from_request` (`:220`) keys
   off a `representations` variable — `[{ "__typename": "User", "id": "1" }]` —
   that today is fabricated by expansion's synthetic `@key`. Source-aware has no
   synthetic `@key`; the parent-data inputs come from the plan's `condition`
   (the Spike-A `FieldSet` on the entering edge) + the classified `inputs`
   (`$this`/`$batch` = parent-data). So the entity path must build
   `ResponseKey` + `RequestInputs` from **(descriptor + planned inputs)** rather
   than from a synthetic-schema `_entities` operation. This is the crux.

## Suggested incremental slices (commit + test each)

1. **Descriptor-driven dispatch lookup.** Add a `coordinate`-keyed connector
   lookup alongside `ConnectorsByServiceName`; wire the source-aware fetch node
   to carry/resolve `coordinate`. Verify: root-field fetch resolves to the right
   `Connector` without a synthetic service name.
2. **Root-field dispatch end-to-end in the router** (not hand-wired). Reproduce
   `steel_thread_root_field_end_to_end`'s result through the actual router path
   for `{ users { id name } }`. Verify against a mock HTTP response.
3. **Entity dispatch from the descriptor.** Build `ResponseKey::Entity` +
   `RequestInputs` from the descriptor's `condition` + `inputs` for
   `{ user(id:"1") { name } }`. This is the multi-commit part; checkpoint often.
4. **`@requires` / cross-source** (`{ user(id) { d } }`, `d @requires(c)`) — the
   plan already sequences these correctly (proven); make the dispatch follow.

## How to verify (reuse what exists)

- **Correctness oracle:** `crate::correctness::check_plan` — used throughout the
  spike diagnostics.
- **Diagnostic tests to model yours on** (all in
  `apollo-federation/src/query_plan/query_planner.rs`, run with `--nocapture`):
  `raw_vs_expanded_plan_diff` (`:1763`), `mirage_check_entity_queries_over_raw_graph`
  (`:1571`), `steel_thread_root_field_end_to_end` (`:1645`).
- **Parity harness:** the `plan-diff` CLI subcommand + `cli/src/plan_diff.rs`
  classifier (`Identical/Equivalent/Different/Error`) is the corpus-level gate;
  its `PlanMode::SourceAware` seam is deliberately pluggable for exactly this
  work.

## Honest scoping note

This slice is where the spike stops being cheap. The planner side turned out
far shorter than the 2024 estimate; the fetch executor is the real,
multi-increment work, and Phase-2 composition-side satisfiability (where the
measured 2–9× expansion blowup actually costs) is untouched beyond it. Treat the
`Equivalent` corpus result as *planning* evidence only — it says nothing about
execution-output identity, which this seam is what finally exercises.
