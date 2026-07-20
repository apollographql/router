# How far off is the source-aware query planner? — spike findings

*Evidence-based distance assessment from the Phase 0/1 spike on
`benjamn/source-aware-phase0`. Companion to the rev-2 proposal
(`claude-h371fb7k:31`) and `PHASE0_HANDOFF.md`. This is a feasibility probe, not
a plan of record.*

## TL;DR

- **The conceptual pieces are proven and cheap.** Connector inputs → edge
  conditions (Spike A), a parity harness (Spike B), the fetch payload
  (`ConnectFetchDescriptor`), and the source-entering edge shape
  (`connect_graph`) all exist, are tested, and were small.
- **The expensive part is unchanged from the 2024 estimate:** making the
  *planner traversal and the router fetch executor* work over a new, leaner
  graph topology. Nothing in this spike de-risked that; it remains the bulk of
  the work (rev-2 Phases 1–4).
- **New quantified motivation:** the synthetic-subgraph expansion inflates the
  query graph **2–9×** (nodes/edges) and **up to ~20×** in key edges vs. the raw
  connector supergraph. That is the ROUTER-1925 compose-time cliff, measured.

## What the spike built (all tested, additive, no core mutation)

| piece | commit | what it proves |
|---|---|---|
| connector input classification + edge-condition derivation | `f8e69b4ea` | connector `$args`/`$this`/`$batch` map to query-graph conditions **without** the expanded schema; differentially matches `resolvable_key` over all expand fixtures |
| `plan-diff` harness + corpus mode | `d8e21f999`, `f9df50b0b` | a parity oracle (semantic via `correctness::check_plan`) exists to gate every later phase; runs a corpus with an aggregate report |
| `ConnectFetchDescriptor` | `8fc412ea9` | the source-aware fetch-node payload (id + selection + inputs + condition) is constructible with no expansion |
| spec-gating fix | `6e12bdff7` | derivation matches the v0.4 vs legacy expander fork |
| `connect_graph` source-entering edge + supergraph collector | `b028b082e`, `568ef5d73` | the Spike-A condition converts losslessly into a canonical `KeyResolution` edge; collected per-supergraph |

## The distance probe (data)

For each expand fixture: build the federated query graph from the **raw**
(non-expanded) connector supergraph, and from the **expanded** supergraph
(today's correct path), and count. Selected rows:

| fixture | raw graph (n/e/key) | expanded graph (n/e/key) | source-aware edges derived |
|---|---|---|---|
| `keys` | 8 / 28 / 3 | 54 / 246 / 64 | 8 |
| `simple` | 13 / 34 / 4 | 24 / 74 / 12 | 2 |
| `interface-object` | 16 / 49 / 8 | 29 / 95 / 18 | 2 |
| `carryover` | 17 / 53 / 6 | 29 / 98 / 14 | 2 |
| `realistic` | 14 / 40 / 1 | 30 / 87 / 4 | 1 |

(`distance_probe_raw_vs_expanded_graph` in `connect_graph.rs` prints the full set.)

### What the numbers say

1. **A raw connector supergraph already builds a query graph.** Its types are
   nodes and its fields are collection edges *before any expansion*. Source-aware
   does **not** need to synthesize type nodes from scratch — a major worry, retired.
2. **What the raw graph lacks is the entity-resolution (`@key`) edges** — expansion
   fabricates those. That is exactly what Spike A/`connect_graph` derive. So the
   "add the connector edges" half of graph construction is largely done.
3. **The expanded graph is 2–9× larger** (up to 6.8× nodes, 8.8× edges, ~21× key
   edges for `keys`). This is the synthetic-subgraph multiplication — the perf
   cliff source-aware removes — now quantified against real fixtures.
4. **The source-aware topology is a third, distinct shape** — not "raw + my
   edges" and not "expanded". It is one source per real connector with a handful
   of source-entering edges, i.e. close to the raw graph's size, not the
   expanded graph's. Defining that topology precisely is the open design work.

## Remaining distance, by rev-2 phase

- **Phase 1 — graph construction: SMALLER than feared.** The raw base graph +
  the derived source-entering edges get most of the way. Open: exact node/edge
  placement for the connector source, `SourceEntering`/`SourceExiting` semantics,
  and root-field (non-entity) connectors. Estimate: **weeks, not months.**
- **Phase 1 — planner integration seam: a small, precise gap.** `QueryPlanner::new`
  (`query_planner.rs:282`) builds its `federated_query_graph` internally via
  `build_federated_query_graph` and stores it in a private field — there is **no
  seam to inject a custom graph**. So even to *test* a source-aware plan you
  first need either (a) a source-aware branch inside `build_federated_query_graph`,
  or (b) a `QueryPlanner` constructor accepting a pre-built `Arc<QueryGraph>`
  (a few lines: mirror `new()`, skip the internal build). This is cheap, but it
  is a prerequisite that does not exist yet.
- **Phase 1 — planner traversal over the new topology: UNPROVEN, the real risk.**
  `query_planning_traversal` + `condition_resolver` must produce a correct plan
  walking this leaner graph. Not attempted in this spike — it requires a full,
  valid source-aware graph (nodes + field-collection edges + root wiring + the
  connector source-entering edges) plus the integration seam above. This is where
  2024 died (`todo!()`), and nothing here de-risks it.
- **Phase 1 — router fetch seam: UNPROVEN.** `make_requests` must build
  `RequestInputs`/`ResponseKey`s from a `ConnectFetchDescriptor` + planned
  operation instead of parsing a synthetic-schema operation
  (`apollo-router/.../connectors/make_requests.rs`). Payload exists; the executor
  rework does not.
- **Phase 2 — composition-side satisfiability:** where the perf payoff actually
  lands (the 2–9× blowup above is a compose-time cost). Untouched.
- **Phases 3–5** (recursion/batching, `@requires`/`@override`/`@context`/abstract
  types, cache + telemetry-name migration): unchanged from the proposal; the
  long tail.

## Steel-thread result (the headline)

With a ~15-line `QueryPlanner::from_query_graph` seam, I drove the **real
planner** over the query graph built from the **raw, non-expanded** steelthread
connector supergraph and planned `{ users { id name } }`. It produced a correct,
non-empty plan with **no expansion and no augmentation**:

```
QueryPlan {
  Fetch(service: "connectors") {
    { users { id name } }
  },
}
```

So for the **root-field (non-entity) connector class, source-aware planning
essentially already works** — the traversal, cost model, and fetch-dep graph
handle the non-expanded connector graph and emit a coherent single fetch. This
is a much shorter distance than the 2024 write-up implied for the easy class.

Two caveats keep this honest:
- The `FetchNode` is a **subgraph-style fetch** (`subgraph_name: "connectors"`),
  not yet a connector HTTP dispatch — turning it into real connector requests is
  the router fetch-seam work (`ConnectFetchDescriptor` is the payload for it).
- This is the *easy* class. **Entity resolution** (the `user(id:)` / `@requires`
  / cross-source cases) needs the connector `@key` edges added to the graph —
  exactly the `build_connector_source_edges` output — and that path is not yet
  exercised end-to-end. That, plus the fetch seam, is the remaining Phase-1 body.

### …and it runs end-to-end (narrow)

`steel_thread_root_field_end_to_end` closes the loop for the root-field class,
deterministically and in-crate (no router service, no network):

1. **Plan** `{ users { id name } }` over the raw connector graph → `Fetch(connectors)`.
2. **Dispatch:** build the connector's real HTTP request via
   `runtime::make_request` → `GET …/users`.
3. **Map:** apply the connector selection `id name` to a mock response array →
   `[{id,name},…]` (extra fields dropped, element-wise over the list).

So for one query class the thread genuinely works plan → request → mapped
result. The manual step here is the plan→connector hand-off (step 1→2), which in
production is the fetch-seam dispatch — the one piece of glue still to build for
this class.

## Mirage check — the most important finding

Skepticism warranted, so I validated with the correctness oracle. Over the
**raw, non-expanded** steelthread graph, all of these plan **and pass
`check_plan`** (`mirage_check_entity_queries_over_raw_graph`):

| query | plan | correctness |
|---|---|---|
| `{ user(id) { name } }` | Fetch(connectors) | ✅ |
| `{ user(id) { c } }` | connectors → graphql (key id) | ✅ |
| `{ user(id) { d } }` — `d @requires(c)` | connectors → graphql → connectors(c) | ✅ |
| `{ user(id) { name d } }` | ✅ | ✅ |

**The existing planner already produces semantically-correct plans for connector
supergraphs over the non-expanded graph** — including `@requires` and
cross-source resolution — because composition emits full join metadata
(`@join__type(key:)`, `@join__field(requires:)`) and the planner **treats
`connectors` as an ordinary subgraph, ignoring `@connect`**. The
synthetic-subgraph expansion is fundamentally an **execution-layer device** (so
connectors *look* like fetchable subgraphs), not a planning necessity.

Where the mirage actually is: those correct plans emit `Fetch(service:
"connectors")` — a **subgraph fetch that no real service backs**. Turning it into
connector HTTP calls is the fetch seam. So the distance relocates:

- **Planner / traversal: largely already works** (validated by `check_plan`).
  The 2024 estimate over-weighted this. Open planner work is narrow: the
  fabricated-structure cases that live *only* in expansion, not in join
  metadata (Implicit-singleton "namespace container", etc.), and confirming
  raw-vs-expanded plan *equivalence* over the corpus (Spike B is built for this).
- **The real bulk is the router fetch seam** (execute `Fetch(connectors)` as
  connector requests — proven end-to-end for the root-field class above) **and
  composition-side satisfiability** (Phase 2, where the 2–9× expansion blowup is
  the compose-time cost).

## Honest verdict

The spike answers "how far off": **the graph-*data* problems are largely solved
and turned out cheaper than the 2024 write-up implied — a raw connector
supergraph is already most of a query graph, and connector conditions derive
cleanly.** The distance that remains is the same distance that has always been
the hard part: **planner traversal + the router fetch executor over a
non-expanded topology**, neither of which this spike touched. Those are the
gating unknowns, and they are real, multi-quarter work. The parity harness
(Spike B, corpus-ready) is in place to measure progress on them the moment a
source-aware plan can be produced.
