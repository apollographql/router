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

- **Planner / traversal: largely already works** (validated by `check_plan`,
  and now across the whole corpus — see the next section). The 2024 estimate
  over-weighted this. Open planner work is narrow: the fabricated-structure
  cases that live *only* in expansion, not in join metadata (Implicit-singleton
  "namespace container", etc.) — which the corpus run below also finds
  Equivalent.
- **The real bulk is the router fetch seam** (execute `Fetch(connectors)` as
  connector requests — proven end-to-end for the root-field class above) **and
  composition-side satisfiability** (Phase 2, where the 2–9× expansion blowup is
  the compose-time cost).

## Corpus-wide plan parity — raw vs expanded

The mirage check validated one fixture. The natural next question is whether
that result is a steelthread artifact or holds corpus-wide. So the parity
oracle Spike B was built for now runs directly: for each operation, plan it
**both ways over the same connector supergraph** and classify the pair with
Spike B's four-way verdict.

- **expansion mode** — today's real path: `expand_connectors` →
  `QueryPlanner::new` on the expanded (synthetic-subgraph) supergraph.
- **source-aware mode** — `QueryPlanner::from_query_graph` over the **raw,
  non-expanded** connector graph.

Each plan is correctness-checked (`check_plan`) against *its own* supergraph +
subgraphs (the raw plan names `connectors`; the expanded plan names synthetic
subgraphs), then the pair is classified:

- **`Identical`** — byte-identical plan text. Won't fire across modes: the two
  name different subgraphs, so text can't match. Kept only for completeness.
- **`Equivalent`** — text differs, but *both* plans are correct against the
  operation → interchangeable at execution. **This is the signal.**
- **`Different`** — plans differ and their correctness verdicts diverge.
- **`Error`** — a mode failed to produce a plan.

`raw_vs_expanded_plan_diff` (in `query_planner.rs`) runs this over every expand
fixture with a usable query surface — **14 fixtures, 32 operations** spanning
root-field, entity, multi-key (`t2(id, id2)`), key-not-selected, nested-entity
chains, deep nested objects, interface objects, abstract inline-fragments
(`... on T1/T2`), the Implicit `namespace` container, batch, and the v0.4
`chained_*` fixtures. (`buggy_graphs_empty` and `directives` are excluded — no
useful query surface.)

**Result: 32/32 Equivalent. 0 Different, 0 Error.**

```
PLAN-DIFF SUMMARY (14 fixtures, 32 ops): identical=0 equivalent=32 different=0 error=0
```

Two things worth calling out:

1. **`namespace` is Equivalent too.** This is the case predicted most likely to
   diverge — the Implicit-singleton "container" structure exists *only* in
   expansion, not in join metadata, so a raw-graph planner has no fabricated
   node to walk. It plans correctly anyway. That retires the last "fabricated
   structure the planner needs" worry flagged above.
2. **Nothing in the corpus diverges.** Across every connector shape in the
   fixtures, planning over the non-expanded graph is never wrong relative to
   expansion planning.

### What Equivalent does and does not prove

`Equivalent` means each plan is *individually* correct against the operation —
its response shape is a subset of the operation's requested shape
(`check_plan`'s semantics). It is **not** proof that the two plans produce
byte-identical execution output. It is the strongest verdict this oracle can
give across two modes that legitimately name different subgraphs, and it is
real evidence, but it is equivalence-*of-correctness*, not
equivalence-*of-output*. Tightening that (a true output-level or
structural-equivalence comparison — the Parallel-as-set / selection-sorting
rules from the old `plan_compare.rs`) is the natural refinement if stronger
parity evidence is ever needed.

**Takeaway:** the "planner ≈ done for connector supergraphs" conclusion is now
corpus-backed, not a single-fixture anecdote. The distance that remains is
unchanged and unmeasured by this experiment: the **router fetch seam** and
**composition-side satisfiability** (Phase 2).

## Phase 1 — the router fetch seam (first slices)

The planner-side spike above ends with correct plans that emit
`Fetch(service: "connectors")` — a subgraph fetch no real service backs. This
section records the first slices of turning that into real connector dispatch
in the router (`PHASE1_FETCH_SEAM_HANDOFF.md` is the full brief). Two slices
have landed, and probing the third relocates the entity-class distance.

### Slice 1 — coordinate-keyed dispatch (landed)

In an *expanded* supergraph every connector becomes its own synthetic subgraph,
so a fetch node's `service_name` uniquely identifies which connector to
dispatch — `ConnectorService::call` keys on it. Source-aware collapses all
connectors into one `connectors` subgraph, so `service_name` is `"connectors"`
for every connector fetch and no longer disambiguates. The
`ConnectFetchDescriptor.coordinate` (`ConnectId::coordinate`) is the stable id
that does. `connectors_by_coordinate()` (`plugins/connectors/query_plans.rs`)
is the lossless re-index dispatch will key on; tested.

### Slice 2 — root-field dispatch through real router code (landed)

`steel_thread_root_field_end_to_end` (apollo-federation) proved
plan → `GET /users` → mapped response for the root-field class, but with the
plan→dispatch step hand-wired *in the federation crate* via
`runtime::make_request`. Slice 2 reproduces the dispatch half through the
**router's** production path: `source_aware_root_field_dispatch`
(`plugins/connectors/make_requests.rs`) drives the supergraph-level operation
`{ users { id name } }` — the exact shape a source-aware plan's fetch node
carries, no synthetic `_entities` operation — through the real
`make_requests`/`root_fields` code and asserts it builds the connector's actual
`GET …/users` request with a `RootField` response key. **The root-field class
now works through router execution code, not just a hand-wired thread.**

### Slice 3 — entity dispatch: the distance is plumbing, not algorithm

Reading the entity path (`entities_from_request`) reframes the remaining entity
work. Two observations:

1. **Entity `RequestInputs` are already synthetic-operation-independent.** For
   each representation `{ "__typename": "User", "id": "1" }`, the inputs are
   built *directly* from that object (`args: rep` for Explicit, `this: rep` for
   TypeSingle). The synthetic `_entities` operation is used **only** to derive
   the response *selection* — not the inputs.
2. **The representations themselves are already accounted for.** Source-aware
   builds each representation from parent data using the entering-edge
   condition (the Spike-A `FieldSet`), not a fabricated synthetic `@key`. Spike
   A's differential test already proved `derive_condition ≡ resolvable_key`
   over the whole corpus — i.e. the condition enumerates exactly the key fields
   expansion fabricates. So "which fields form the representation" is a solved,
   tested question.

What remains for the entity class is therefore **executor plumbing, not a new
algorithm**:

- Derive the entity response *selection* from the descriptor's
  `output_selection` (supergraph-schema selection) instead of from
  `apply_selection_set` over a synthetic `_entities` operation.
- Thread the `ConnectFetchDescriptor` onto the source-aware fetch node and into
  `make_requests`, and have the executor build representations from parent data
  via the condition. This is genuine cross-crate work — it needs
  `ConnectFetchDescriptor` exposed beyond `apollo-federation`, a fetch-node
  field, and changes to core execution — and it is where, per the handoff, "the
  spike stops being cheap."

Net: the entity class has **no unproven algorithmic content left** (inputs +
key-fields are both already validated); its distance is the descriptor-threading
plumbing through the executor. That is a real, higher-blast-radius chunk, but a
*known* one — not a research risk.

## Honest verdict

The spike answers "how far off": **the graph-*data* problems are largely solved
and turned out cheaper than the 2024 write-up implied — a raw connector
supergraph is already most of a query graph, and connector conditions derive
cleanly.** The distance that remains is the same distance that has always been
the hard part: **planner traversal + the router fetch executor over a
non-expanded topology**. Planner traversal turned out largely already-working
(corpus-backed above). The fetch executor is no longer entirely untouched: the
**root-field class now dispatches through real router code** (Slice 2), and
probing the **entity class** showed its remaining distance is descriptor-
threading *plumbing* through the executor, not unproven algorithm (Slice 3
analysis) — a known, higher-blast-radius chunk rather than a research risk.
What is still genuinely unbuilt and multi-quarter: that entity-path executor
plumbing end-to-end, and **composition-side satisfiability** (Phase 2), which
this spike has not touched at all. The parity harness (Spike B, corpus-ready)
remains in place to measure progress the moment a full source-aware plan
executes.
