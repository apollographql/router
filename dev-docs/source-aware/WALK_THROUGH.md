# Source-aware query planning: a walk-through

`README.md` in this directory is the reference: it answers "what is true about
this branch." This file answers a different question, "what order do I learn
this in," and it is meant to be read aloud or followed link by link while
explaining the work informally.

Ten sections. Each one names the file or test that proves it, so nothing here
has to be taken on trust.

---

## 1. The comment that started it

Open [`apollo-federation/src/connectors/mod.rs`](../../apollo-federation/src/connectors/mod.rs)
and find `ConnectId::synthetic_name`:

> Until we have a source-aware query planner, we'll need to split up connectors
> into their own subgraphs when doing planning.

The comment above `expand_connectors` in
[`connectors/expand/mod.rs`](../../apollo-federation/src/connectors/expand/mod.rs)
opens with the same clause, restates the problem as needing "to interface
with standard query planning concepts while still enforcing
connector-specific rules," and lands on the same conclusion: "each connector
is separated into its own unique subgraph." Both are present tense on `dev`
today. Everything below is what happened when somebody tried to cash that
check.

## 2. The trade

Connectors shipped by making themselves look like ordinary subgraphs. For every
`@connect` directive, `expand_connectors` synthesizes a minimal subgraph: it
walks the connector's selection to get the output shape, emits a type exposing
only the fields that connector returns, gives it a unique name, fabricates
`@key` directives, and assigns `@join__field` homes.

Two things get encoded there, and they are the whole story:

- the connector's **identity** becomes the synthetic subgraph name
- the connector's **boundary**, meaning which fields it can serve, becomes that
  subgraph's field set

Both structural, both in the schema. It was a good trade. It bought a shipped
product without touching composition, satisfiability, planning or execution.

## 3. The bill

The bill is paid in two static currencies, query-graph size and composition
cost, at composition time and at router startup, before a single request is
served. The second is dominated by a single phase: satisfiability.

`distance_probe_raw_vs_expanded_graph` in
[`query_graph/connect_graph.rs`](../../apollo-federation/src/query_graph/connect_graph.rs)
measures the first against the fixture set: growth is a shape-dependent
multiple, from nothing (no entity structure to duplicate) up to 8.8x edges
and 21x key edges for `keys`. Treat those as illustrations, not a
coefficient — the multiplier is a function of the schema's connectors and
entities. What survives asymptotically is the count: expansion makes logical
subgraphs proportional to `@connect` directives, and everything downstream
scales in that count.

The second was measured on a real graph. Constellation staging's
6 connector subgraphs expand into **1,398 synthetic ones**, and
satisfiability over the expansion is essentially the whole composition bill
— **~14 GB and ~260 seconds** in the Rust check alone, versus under 0.6 GB
for every other phase combined. Subset sweeps put satisfiability at roughly
**N^2.5 in memory and N^3.5 in time** in synthetic-subgraph count. (PR #9663
buys time — 0.27 GB / 0.6 s, identical verdict — by merging subgraphs that
share a resolvable-key signature; it shrinks nothing the router or planner
sees.)

The fleet is not at that wall yet. Across the ~7,300 connector-using graphs
in the May 2026 corpus snapshot: median 1 connector, p99 54, max 370, against
Constellation's 1,300+ — runway bought by the expansion trick, cut short
(thankfully) by our own dogfooding, and shrinking at N^3.5. And the wall is
already operational, not just compositional: with one composed graph unable
to carry 1,300+ connectors, Constellation is split across separate routers,
with orchestration bolted on above (MCP, for now). The composition ceiling
becomes deployment architecture.

The quieter cost is the one that matters more. The planner's cost model prices
**subgraph boundary crossings**. One HTTP backend that expansion splits into
six synthetic subgraphs is priced as six independent systems, so the cost
model cannot express the question that actually matters for a connector plan:
how many distinct external systems does this plan touch?

## 4. The false summit

The plan was to write a new planner. That turned out to be unnecessary.

A roughly fifteen-line seam, `QueryPlanner::from_query_graph`, was enough to run
the **existing** planner over the raw, non-expanded connector supergraph. It
produced a correct plan immediately, with no expansion and no augmentation.

That was suspicious enough to warrant trying to falsify it.
`raw_vs_expanded_plan_diff` in
[`query_plan/query_planner.rs`](../../apollo-federation/src/query_plan/query_planner.rs)
plans each operation both ways and classifies the pair through the correctness
oracle `crate::correctness::check_plan`. Result: 39 operations, 39 `Equivalent`,
0 `Different`, 0 `Error`.

**Be precise about what that is.** The test calls itself a "spike diagnostic,
subset" in its own doc comment, and it earns the label: 39 hand-written
operations across 14 of the 18 expand fixtures, several of them a single query,
against fixtures that exist to test the very thing being removed. Call it a
**viability suite**, not a corpus.

It killed one hypothesis decisively, which was its job:

> Synthetic-subgraph expansion is an execution-layer device, not a planning
> necessity.

It is not a safety argument, and section 7 explains why nothing of that shape
could be.

**The generalization is the thesis of everything below.** The planner's search
was never the hard part. The hard part is what the graph is able to *say*: the
bill in section 3 is information the cost model cannot express, the failures in
section 5 are a boundary the graph does not record, and section 8 is a
dependency federation has no syntax for at all. Every remaining section is a
consequence of that one sentence.

## 5. The mirror pair

The same failure, in both directions, from one design decision. Both are silent
nulls. Neither produces an error anywhere in the pipeline.

**Left: what collapsing the graph does.** In the raw graph there is one `User`
node carrying every field, so the planner folds `username` into a fetch whose
connector returns only `id name`. Expansion was *physically incapable* of this,
because a minimal synthetic subgraph cannot serve a field it does not return.

Status: **observed, fixed, and pinned twice.** The fix is
`restrict_connector_reachability` in
[`query_graph/connect_graph.rs`](../../apollo-federation/src/query_graph/connect_graph.rs).
It is pinned at the plan-shape level by
`plans_entity_resolver_fetch_for_unprovided_field` in
[`query_plan/source_aware.rs`](../../apollo-federation/src/query_plan/source_aware.rs)
and at the wire level by `source_aware_entity_resolver_connector_gap` in
[`plugins/connectors/tests/mod.rs`](../../apollo-router/src/plugins/connectors/tests/mod.rs),
which runs both paths against mock servers and asserts identical dispatched
requests.

**Right: what expansion does.** One connector, one selection:

```graphql
Query.users: [User]
  @connect(selection: "id manager { id name } reports { id }")
```

`Person` arrives under `manager` with `{id, name}` and under `reports` with
`{id}`. Expansion has one `Person` per synthetic subgraph, so it unions the
positions and attributes `Person.name` to a connector that does not return it
under `reports`.

| query | expansion | source-aware |
|---|---|---|
| `{ users { manager { name } } }` | `"Ada"` | `"Ada"` |
| `{ users { reports { name } } }` | **`null`** | `"Grace"` |

Status: **live, and enshrined.** The reproduction is
[`apollo-router/tests/samples/connectors/sibling-position-over-merge/`](../../apollo-router/tests/samples/connectors/sibling-position-over-merge/),
which runs both paths in one test via `ReloadConfiguration`. Its expansion-path
expectations encode the defect deliberately. If expansion is ever fixed, that
sample should fail loudly rather than be quietly updated.

The fixture values are chosen so the test cannot pass for the wrong reason.
`GET /people/9` returns `"Ada (via resolver)"`, not `"Ada"`, so `manager { name }`
coming back as plain `"Ada"` shows the restriction did not manufacture a needless
entity fetch at a position the connector can already serve. `"Grace"` appears
nowhere in the `/users` response, so it cannot be an over-merge artifact.

**The contrast worth stating out loud:** one side's test asserts the fix, the
other side's test asserts the bug. Expansion's instance ships in production
today, composes without a single hint from Rover, and its own suite records the
wrong answer as expected.

So: expansion explodes too much for performance and not enough for correctness,
and it is the same decision both times.

## 6. Is the representation actually safe?

The representation is *allowed*. That is not the same as knowing it will not be
buggy, and this is the question the rest of the work is organized around.

Everything up to here says "it works." Everything after says why that sentence
is not worth much on its own.

## 7. What we found when we looked

At least three planner optimizations assume **the same type means the same
fields are reachable**. Boundary copies deliberately break that. All three were
found by hitting them, not by predicting them:

1. The indirect-search guard skipped an edge back to the search's original
   source, on the grounds that the direct transition "was already checked." That
   is false for a pruned copy.
2. Detour elimination (`check_direct_path_from_node`) found a direct path ending
   on the pruned copy itself and discarded the re-entry as a useless detour,
   because it compares nodes by type.
3. The "fully local selection" shortcut in
   [`query_plan/query_planning_traversal.rs`](../../apollo-federation/src/query_plan/query_planning_traversal.rs).

The first two live in
[`query_graph/graph_path.rs`](../../apollo-federation/src/query_graph/graph_path.rs).
The umbrella: the planner caches conclusions about a node, and boundary copies
make the cache lie.

Note that the third is a **different category**. The first two eat a path; that
one skips path-building altogether. Which is why "no plan option disappeared" is
not evidence that no assumption was tripped.

## 8. Where the raw graph simply cannot say it

This is the most original finding on the branch, and it is the one that changes
what the end state is.

A connector's mapping expressions can read sibling fields of their own parent
type through `$this`. That is a real data dependency: the fetch cannot be
dispatched until those reads are resolved. Expansion learns this by accident.
`Connector::resolvable_key` derives the synthetic subgraph's `@key` from
`variable_references()` across transport and selection, which is why
`simple.graphql`'s `User.d` expands to key `"b c"` while declaring only
`requires "c"`.

Source-aware plans over the raw graph, where nothing encodes those reads, so it
satisfies only the declared `@requires` and never fetches `b` at all.

The fix half exists and is **deliberately not wired in**.
`apply_connector_parent_conditions` does make the planner fetch the reads, but
with it applied `correctness::check_plan` rejects the plan, and the rejection is
right: `@requires` names external fields, and a sibling field of the same
subgraph is not external.

**Federation has no way to say "this field needs that sibling field of the same
subgraph."**

Expansion escapes this only because splitting each connector into its own
synthetic subgraph makes the sibling dependency cross-subgraph, at which point it
is expressible as a key. So the synthetic-subgraph split is not only a cost. **It
also buys the vocabulary.** The gap is recorded by
`plans_this_variable_reads_as_fetch_inputs`, which asks the connector what it
needs and the plan what it supplies, so it carries no snapshot to rot and starts
passing on its own if the gap ever closes.

## 9. How much of this is real, out in the corpus

The viability suite in section 4 cannot answer this, and neither can any test
whose oracle is parity with expansion: parity is structurally incapable of
judging a case where expansion is the one that is wrong.

So the shapes were counted directly, across the 7,375 graphs of the fleet corpus
(real customer schemas alongside demos, sandboxes and Apollo's own tutorial
graphs, so "customer graphs" would overstate it)
(`~/dev/connectors-corpus-may-2026`, snapshot of 2026-05-06) using
`extractor/src/bin/probe.rs`, which runs the whole set in about five seconds.
The corpus and the probe are internal to Apollo, which makes this the one
section a reader cannot re-run from the repo alone; every number in it should
be read with that caveat attached.

Those 7,375 files collapse to **5,422 distinct schema families**, since some
graphs appear as more than a hundred near-identical variants. Family counts are
the honest denominator.

| shape | graphs | families | of 5,422 |
|---|---|---|---|
| `$this` read of a field outside an existing `@key` (section 8) | 244 | 13 | 0.24% |
| sibling-position over-merge (section 5, right) | 298 | 21 | 0.39% |
| connector under-provides against a keyless type | 732 | 514 | 9.5% |

24,380 connectors were scanned. The selection reader refuses to half-parse,
so 4.6% of selections were skipped rather than guessed at, which makes the
second and third rows **lower bounds**. The first row does not depend on that
reader: it is a plain text scan of the whole directive, checked against keys
parsed from the SDL, so it skips nothing.

Representative hits, all of them the same shape as the fixture in section 5:

```
dell-services   Mutation.validateServiceablePart
                ServiceableAssetPart wide at part.part, narrow at part.substituteParts
                fields at risk: base, commodity

dell-services   Asset.partsPath          reads $this.manufacturer, $this.serialNumber
hyatt-180b      Property.socialCarousel  reads $this.spiritCodeComposite
n-able          Partner.readyInvoices    reads $this.sfdcId
wownz           Product.inventory        reads $this.storeKeyPrefix
```

**Two honest qualifications.** These are graphs that *contain the shape*, not
graphs observed returning `null`: the bug fires when someone queries the missing
field at the narrow position. And the population is mixed, with real customer
schemas sitting alongside demos, sandboxes and Apollo's own tutorial graphs.

## 10. What is actually being claimed, and what comes next

Not "it is not buggy." Three narrower things, each doing real work:

**Containment is structural, not conditional.** The traversal relaxations gate on
`QueryGraphNode::connector_boundary_copy`, a marker only the source-aware pass
ever sets. In an expansion graph the copy set is empty by construction, so the
new paths are unreachable rather than skipped by a conditional. A federation user
with no connectors cannot reach that code.

**Verdicts are computed, not eyeballed.** `check_plan`, the `plan-diff` CLI in
[`apollo-federation/cli/src/plan_diff.rs`](../../apollo-federation/cli/src/plan_diff.rs),
and the fixture set make "equivalent" a computed classification.

State the limit of that honestly: `check_plan` is **one oracle, and a disputed
one**. There are known operations where the planner emits a plan its own checker
rejects, and nothing in the system currently adjudicates which of the two is
right. So section 4's verdicts should be read as one line of evidence, not the
foundation. Three of the evidence classes here do not depend on it at all:

- the **wire-level end-to-end tests**, which run both paths against mock servers
  and compare dispatched requests rather than plans;
- the **corpus prevalence probes** in section 9, which read schema shapes and do
  no planning whatsoever;
- the **self-reporting invariants** below, which report a hazard rather than
  asserting its absence.

Worth noting in the same breath: the one place on this branch where `check_plan`
and the planner disagreed, in section 8, the checker was **right**, and the code
deferred to it by leaving `apply_connector_parent_conditions` unwired.

**Unproven invariants report themselves.** The best example is in
[`query_plan/fetch_dependency_graph.rs`](../../apollo-federation/src/query_plan/fetch_dependency_graph.rs),
at the unguarded `can_rebase_on`. It answers "can the parent fetch this" from the
parent's *subgraph schema*, which a boundary copy invalidates. No known operation
reaches the hazardous combination, so rather than change plan shape on a hazard
with no reproduction, it emits a warning. Its comment states the philosophy
directly: an unproven invariant that reports itself is worth more than one nobody
would notice breaking. Silent on every expansion graph, by construction.

All three are detection rather than confidence, for one reason: **the failure
mode is silence.** A path quietly disappears, or a field comes back `null`.
Nothing errors.

**Current state.** Two triggers, not one. The config flag
`experimental_connectors_source_aware` is off by default, and separately a
connect **v0.5** supergraph is never expanded, so it is always planned
source-aware. The spec version in the developer's `@link` is the single place
that is configured, visible both to composition, which relaxes
`CircularReference`, and to the router at schema load, which skips expansion.
Both fork sites are in
[`apollo-router/src/spec/schema.rs`](../../apollo-router/src/spec/schema.rs) and
[`apollo-router/src/query_planner/query_planner_service.rs`](../../apollo-router/src/query_planner/query_planner_service.rs),
each carrying a comment pointing at the other. Adoption is measured by two gauges
in [`apollo-router/src/configuration/metrics.rs`](../../apollo-router/src/configuration/metrics.rs),
because the config path and the schema-derived path cannot be covered by one.

**The ceiling.** Rows 2 through 4 of the replacement table in `README.md` are
patches at three different layers: a plan-time fetch split, a graph pass, and a
stamping match. The uniform fix is **typed source identity in the query graph**,
so the boundary lives in the model instead of in three patches. It pays both
bills: section 7's, by making the planner unable to forget the boundary, and
section 8's, by letting the graph express an intra-source dependency without
splitting the source.

Follow-ons in order: no-key semantics, nested output shapes (recursion into
object-typed fields, which is distinct from the nested *positions* work already
done), type-level entity resolvers, multi-connector variants, then interfaces and
unions as connector output.

The comment in section 1 says *until we have a source-aware query planner*. We
now know roughly what that sentence costs.
