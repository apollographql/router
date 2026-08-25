# Source-aware query planning

Today the router turns a connector supergraph into one synthetic subgraph per
`@connect` directive, then plans over the result as if it were an ordinary
federated schema. Source-aware planning removes that step: the planner works
directly on the raw supergraph, and each fetch carries the identity of the
connector that serves it.

This directory documents that work.

This file is the **reference**: what is true about the branch, section by
section. If you are meeting this work for the first time, or explaining it to
someone, read [`WALK_THROUGH.md`](WALK_THROUGH.md) instead, which covers the
same ground in the order it is best learned and links the file or test behind
each claim.

## Status

| | |
|---|---|
| Triggers | **two, not one.** `experimental_connectors_source_aware`, default off; and separately a connect **v0.5** supergraph, which is never expanded and so is always planned source-aware regardless of config |
| Flag off, v0.4 and earlier | byte-identical to today's expansion path |
| Flag on | plans over the raw supergraph and dispatches real connector HTTP requests. **No longer always identical to expansion** — see "Where the two paths diverge" below |
| Last verified | **2026-08-25 at [`d4e4da747`](https://github.com/apollographql/router/commit/d4e4da747d7320b77db35cba8cb9b29b4a1485d4)**: `raw_vs_expanded_plan_diff` (39 ops, 39 Equivalent), `distance_probe_raw_vs_expanded_graph`, the `sibling-position-over-merge` sample, and the full `apollo-federation` suite (1948 + 838, 0 failed) all run live, the suite after rebasing onto `origin/dev`. Older and not re-confirmed since: the router-side connectors suite, and clippy/rustfmt. Re-run the recipe in "Working on it" before relying on the unconfirmed half |
| Not built | source-aware cost model, composition-side satisfiability, type-level entity resolvers, interfaces/unions as connector output |
| Recently built | **nested output *positions*** — one type reached at several positions with different sub-selections now gets a restricted node per position (`SOURCE_AWARE_NESTED_POSITIONS.md`). Distinct from nested output *shapes*, which is recursion into object-typed fields and is still unbuilt; see "What is not built". This also makes `User.friends: [User]` plannable, and under connect v0.5 validation now permits it (`CircularReference` is enforced only through v0.4) |
| Tracking | exploratory, no engineering ticket |

This is a feasibility spike that outgrew its original scope. It is not queued
to ship, and the "What is not built" section below is a deliberate account of
its limits rather than a list of oversights.

## Why this exists

### The trade that started it

Connectors shipped by making themselves look like ordinary subgraphs, so that
composition, satisfiability validation, query planning and execution could be
reused without modification. The code says so directly, in
`apollo-federation/src/connectors/mod.rs`:

> Until we have a source-aware query planner, work with connectors will need to
> interface with standard query planning concepts while still enforcing
> connector-specific rules. To do so, each connector is separated into its own
> unique subgraph [...] This allows satisfiability and validation to piggy-back
> off of existing functionality in a reproducible way.

That was a good trade. It bought a shipped product without touching the code
the rest of the router depends on most heavily.

### What expansion actually does

For every `@connect` directive, `expand_connectors`
(`apollo-federation/src/connectors/expand/mod.rs`) synthesizes a minimal
subgraph. It walks the connector's `selection` to get the output shape, emits a
type exposing only the fields that connector returns, gives it a unique subgraph
name, fabricates `@key` directives for the entities it resolves, and assigns
`@join__field` homes.

Two things get encoded there, and they are the whole story:

- the connector's **identity** becomes the synthetic subgraph name
- the connector's **boundary**, meaning which fields it can serve, becomes that
  subgraph's field set

Both are expressed structurally, in the schema.

### What it costs

The explosion is per directive, not per backend. `distance_probe_raw_vs_expanded_graph`
(in `apollo-federation/src/query_graph/connect_graph.rs`) measures it against the
expand fixtures. The worst case is `keys`, at **6.8x nodes, 8.8x edges and 21x
key edges** (8 nodes to 54, 28 edges to 246, 3 key edges to 64). Most fixtures
land between 1.5x and 3x, and a few with no entity structure to duplicate
(`types_used_twice`, `nested_inputs`, `recursive_input`) do not grow at all.
That inflation is paid at composition and at router startup.

There is a second, quieter cost. The planner's cost model prices boundary
crossings between subgraphs. One HTTP backend that expansion splits into six
synthetic subgraphs is priced as six independent systems, so the cost model
cannot express the question that actually matters for a connector plan: how many
distinct external systems does this plan touch?

## The central finding

The original plan was to write a new planner. That turned out to be unnecessary.

A roughly fifteen-line seam, `QueryPlanner::from_query_graph`, was enough to run
the **existing** planner over the raw, non-expanded connector supergraph. It
produced a correct plan immediately, with no expansion and no augmentation.

That result was suspicious enough to warrant an explicit attempt to falsify it.
`mirage_check_entity_queries_over_raw_graph`
(`apollo-federation/src/query_plan/query_planner.rs`) runs the hard classes
through the correctness oracle `crate::correctness::check_plan`:

| query | plan | correct |
|---|---|---|
| `{ user(id) { name } }` | `Fetch(connectors)` | yes |
| `{ user(id) { c } }` | connectors then graphql, by key `id` | yes |
| `{ user(id) { d } }`, `d @requires(c)` | connectors, graphql, connectors | yes |
| `{ user(id) { name d } }` | | yes |

`raw_vs_expanded_plan_diff` then widened it, though not as far as "the corpus"
would suggest: the test calls itself a "spike diagnostic, subset" and it is one.
**39 hand-written operations across 14 of the 18 expand fixtures**, several of
them a single query, planned both ways and classified by the `plan-diff` verdict
(`Identical` / `Equivalent` / `Different` / `Error`). Read it as a *viability
suite*, not a corpus: it covers what its author thought to write down, over
fixtures that exist to test the very mechanism being removed.

**Result: 14 fixtures, 39 operations, 39 Equivalent, 0 Different, 0 Error.**
(Re-run live on 2026-08-25 at [`d4e4da747`](https://github.com/apollographql/router/commit/d4e4da747d7320b77db35cba8cb9b29b4a1485d4).)

That spans root-field, entity, multi-key, key-not-selected, nested-entity
chains, deep nested objects, interface objects, abstract inline fragments, the
Implicit `namespace` container, batch, and the v0.4 `chained_*` fixtures. The
`namespace` fixture matters most: its singleton container structure exists only
in expansion, never in join metadata, so it was the case most likely to diverge.
It came back Equivalent.

**Why it works:** composition already emits full join metadata
(`@join__type(key:)`, `@join__field(requires:)`), and the planner ignores
`@connect` entirely. It just sees a subgraph named `connectors`.

> Synthetic-subgraph expansion is an execution-layer device, not a planning
> necessity.

### What `Equivalent` does and does not prove

`Equivalent` means each plan is individually correct against the operation: its
response shape is a subset of the requested shape, which is what `check_plan`
verifies. It is **not** proof that the two plans produce byte-identical
execution output. Across two modes that legitimately name different subgraphs,
it is the strongest verdict this oracle can give, and it is real evidence, but
it is equivalence of correctness rather than equivalence of output.

### Where the two paths diverge, on purpose

Every check above uses **parity with expansion as the oracle**, and for most of
this branch that is right. There is now one class where it is not.

A connector whose selection reaches one type at several positions with different
sub-selections — `id manager { id name } reports { id }`, where `Person` arrives
under `manager` with `{id, name}` and under `reports` with `{id}` — is a shape
**expansion cannot represent**. It has one `Person` per synthetic subgraph, so it
unions the positions and attributes `Person.name` to a connector that does not
return it under `reports`. The field comes back `null`, on a schema rover
composes without errors or hints.

Source-aware planning gives each position its own restricted node and resolves
`name` through the entity resolver. So here the two paths **must** produce
different answers, and source-aware's is the correct one.

Two consequences, both easy to get wrong later:

- **Parity is not the test for this class.** A source-aware result that matches
  expansion on such a schema is the bug, not the confirmation. Before using the
  parity oracle on a new case, ask whether expansion gets that case right.
- **The reproduction is `apollo-router/tests/samples/connectors/sibling-position-over-merge/`**,
  which runs both paths in one test via `ReloadConfiguration` and asserts the
  divergence directly. Its expansion-path expectations encode a defect
  deliberately; if expansion is ever fixed, that sample should fail loudly rather
  than be quietly updated.

"Flag off is byte-identical to expansion", on a v0.4-or-earlier supergraph,
remains true and is unaffected — the
restricted nodes only exist in a graph the source-aware pass has mutated.

## Priors worth updating

If you have worked on connectors or the query planner, several assumptions this
area rests on turned out to be wrong. They are collected here because each one
changed what the work was, and because holding any of them makes the rest of
this document read strangely.

**"Connectors need a new query planner."** They need a modified graph and a way
to carry connector identity. The existing planner, its traversal, its cost model
and its fetch-dependency graph all handle a raw connector supergraph as-is. The
project is not building a planner; it is re-supplying, as data, what expansion
was carrying structurally.

**"Expansion is required for planning and satisfiability."** It is an
execution-layer device. Composition already emits the join metadata the planner
needs, and the planner ignores `@connect` entirely. Expansion exists so that
connectors *look* fetchable, not so that they can be planned.

**"Planner traversal is the hard part."** This was the 2024 estimate, and it is
where the 2024 attempt stopped. Traversal turned out to be largely already
working. The remaining distance is the router fetch seam and composition-side
satisfiability, neither of which is traversal.

**"The query graph is one node per type."** It has never been.
`build_query_graph`'s `@provides` handling already copies a type's node and
re-points a field's in-edge at the copy. Multi-node-per-type is established
machinery, which is the only reason the restrictive-provides fix was spike-scale
instead of a graph rewrite.

**"If two nodes have the same type, the same fields are reachable from both."**
At least two planner optimizations depend on this, and boundary copies
deliberately violate it. One guard skipped an edge because the direct transition
"was already checked"; detour elimination discarded a re-entry because it found a
same-typed node, which was the pruned copy itself. Expect to find more of these
when extending the pass, and expect them to present as a path silently
disappearing rather than as an error.

**"The cost model prices backend calls."** It prices subgraph boundary
crossings. Under expansion those happen to correlate, because each connector is
its own subgraph. Collapse them into one `connectors` subgraph and the signal
vanishes entirely: a plan making six HTTP calls to the same backend looks free.
Worse, expansion's model actively prefers reaching out to a *second* backend when
that backend batches, because batching reduces the fetch count it is counting.

**"Expansion's explosion is purely a performance problem, so removing it is
purely a win."** No. The minimal synthetic subgraph made it physically
impossible to route a field to a connector that does not return it. Collapsing
that away is what introduced the over-merge and the silent nulls. Expansion's
explosion was doing correctness work, and source-aware has to pay for it
deliberately. Row 3 in the table below is that bill, and it is not fully paid.

**"Expansion's synthetic subgraphs only ever cost us something."** They also
bought *vocabulary*. A connector's mapping expressions can read sibling fields of
their own parent type through `$this`, and that is a real data dependency: the
fetch cannot be dispatched until those reads resolve. Expansion learns it by
accident, because `Connector::resolvable_key` derives the synthetic subgraph's
`@key` from `variable_references()`, which is why `simple.graphql`'s `User.d`
expands to key `"b c"` while declaring only `requires "c"`. Splitting each
connector into its own subgraph makes an intra-connector dependency
*cross-subgraph*, at which point federation can express it as a key. Collapse the
split and the dependency becomes unsayable: `@requires` names external fields, and
a sibling field of the same subgraph is not external. See "What is not built".

**"The corpus result means source-aware and expansion produce the same plans."**
It means each plan is independently correct against the operation. See "What
`Equivalent` does and does not prove" above.

**"Self-edges can carry a re-entry."** The planner ignores query-graph edges
whose head equals their tail, except for `@defer`. Grafting a re-entry edge onto
a node without first copying the node does nothing at all, silently.

## What expansion gave us for free

If expansion was doing several jobs at once, collapsing it means each job
becomes something to reconstruct deliberately. This accounting is the project
plan.

| | what expansion provided | source-aware replacement | state |
|---|---|---|---|
| 1 | **Dispatch identity**, via a unique `service_name` per connector | a coordinate carried on the fetch node | done |
| 2 | **Fetch decomposition**, since distinct subgraphs become parallel fetch siblings | a plan-time split into per-connector fetches | root-field only |
| 3 | **Field-availability boundaries**, since a minimal subgraph physically cannot serve a field it does not return | the restrictive-provides graph pass | top-level shapes only |
| 4 | **Entity-resolver identity and keys** | stamping by entity output type; keys were already on the raw schema | `Explicit` resolvers only |
| 5 | **Input sequencing** for `@requires` | nothing needed, it lives on the raw schema | done |
| 6 | **Cost granularity** | the one place expansion was actively harmful, and the opportunity | not built |
| 7 | **Observability granularity**, via subgraph-keyed spans and metrics | reattach via the carried coordinate | not surveyed |

Rows 1 through 5 are one problem in different clothes. Expansion encoded
per-connector structure into the schema; source-aware has to carry that
structure some other way. Row 6 is the only place the side effect was harmful,
and where source-awareness is a net improvement. Row 7 is a labeling task that
nobody has looked at yet.

## What is built

### The identity channel

The architectural principle: synthetic subgraph names were an identity-carrying
device, so replace them with a lighter one rather than reverse-engineering
identity later.

- `FetchNode.connector: Option<String>` on both the federation
  (`apollo-federation/src/query_plan/mod.rs`) and router
  (`apollo-router/src/query_planner/fetch.rs`) fetch nodes, threaded through
  `apollo-router/src/query_planner/convert.rs`. It is `None` by default,
  serde-skipped, and absent from `Display`, so existing plans serialize
  unchanged.
- `stamp_connector_coordinates` (`apollo-federation/src/query_plan/connector_stamp.rs`)
  sets it at plan time by matching each fetch's target `(type, field)` against
  the ground-truth `Connector` set. Determined once from `@connect` metadata,
  never guessed at runtime.
- `resolve_connector` (`apollo-router/src/plugins/connectors/query_plans.rs`)
  prefers the carried coordinate at dispatch and falls back to service name.

### The pipeline

The entire schema-build divergence is a single call site: the
`expand_connectors` call in `apollo-router/src/spec/schema.rs`. That call does
two jobs at once, rewriting the schema for planning and building the connector
index. Source-aware splits them: keep the raw SDL for planning, and build a
coordinate-keyed index instead of a service-name-keyed one.

Downstream, three consumers change. The planner
(`apollo-router/src/query_planner/query_planner_service.rs`, `create_planner`)
builds via `SourceAwareQueryPlanner` and stamps the resulting plan. The
connector service factory receives the by-coordinate index. Dispatch
(`apollo-router/src/services/fetch_service.rs`, `apollo-router/src/services/connector_service.rs`)
resolves by coordinate.

A useful simplification surfaced here: source-aware planning is the ordinary
planner with `validate_extracted_subgraphs` disabled, plus the stamp. There is
no separate planner.

Covered end to end by `source_aware_root_field_end_to_end` and
`source_aware_entity_plus_requires_end_to_end` in
`apollo-router/src/plugins/connectors/tests/mod.rs`, both asserting equality
with the expansion path on both the response and the dispatched requests.

### Fetch decomposition (row 2)

A single `connectors` fetch can merge fields from several connectors, because
fetch merging keys on subgraph name and they now share one. `split_root_field_fetch`
(in `connector_stamp.rs`) reconstructs the decomposition in the plan: when a
`connectors` fetch's top-level fields span more than one connector, the single
`Fetch` is replaced by a `Parallel` of per-connector fetches. Same executor, the
boundaries are simply re-established at plan time.

Covered by `source_aware_multi_connector_merged_fetch_end_to_end`. Entity-field
merges and variable-bearing merges are deliberately left unsplit.

### Restrictive provides (row 3)

This is the sharp one. In the raw graph there is one `User` node carrying every
field, so the planner folds `username` into a fetch whose connector returns only
`id name`, and the result is a silent `null`.

`restrict_connector_reachability`
(`apollo-federation/src/query_graph/connect_graph.rs`) models the boundary in
the graph itself. Per connector field-collection edge it copies the landing-type
node, prunes the copy's out-edges to `connector_provided_fields()` plus
`__typename` plus the key fields, re-points the field's in-edge to the copy, and
lets the original's self-key edge clone into a `KeyResolution` re-entry from the
copy back to the full node. The planner then does the right thing on its own:

```
Sequence {
  Fetch(connectors) { users { __typename id } }
  Flatten(users.@) { Fetch(connectors) { _entities { ... on User { username } } } }
}
```

The key realization behind it is that `build_query_graph` is already not
one-node-per-type. `@provides` handling copies a type's node and re-points a
field's in-edge to the copy. A connector is a field with an implicit
`@provides(<its output shape>)`, except the copy has to be restrictive rather
than additive.

**The general lesson, which the follow-on work will keep meeting:** at least two
planner optimizations assume that *the same type means the same fields are
reachable*. Boundary copies deliberately break that.

1. The indirect-search guard skips edges back to the search's original source,
   on the grounds that the direct transition was already checked. That is false
   for a pruned copy.
2. Detour elimination (`check_direct_path_from_node`) found a "direct path"
   ending on the pruned copy itself and discarded the re-entry as a useless
   detour, because it compares nodes by type.

Both relaxations are gated on `QueryGraphNode::connector_boundary_copy`, a
marker only the source-aware pass ever sets. Post-build mutation also
invalidates `non_trivial_followup_edges` and `non_local_selection_metadata`, so
the pass recomputes both.

Covered by `plans_entity_resolver_fetch_for_unprovided_field`
(`apollo-federation/src/query_plan/source_aware.rs`) at the plan-shape level and
`source_aware_entity_resolver_connector_gap` end to end.

## How it stays safe

Six properties, each doing real work:

**Two triggers, one helper.** `experimental_connectors_source_aware` is off by
default, and a connect v0.5 supergraph is never expanded, so v0.5 is planned
source-aware whether or not the flag is set. The spec version in the developer's
`@link` is the single place that is configured: it is visible to composition,
which relaxes `CircularReference`, and to the router at schema load, which skips
expansion, so there is no second knob to keep in sync. Both router fork sites
read one helper and each carries a comment pointing at the other, because
`spec/schema.rs` decides whether `raw_sdl` holds the raw or the expanded
supergraph and planning the wrong one against the wrong planner would be
silently incorrect. Schema load logs when the switch came from the spec version
rather than from configuration. With the flag off on a v0.4-or-earlier
supergraph, `expand_connectors` runs exactly as it does today.

Adoption is measured by two gauges in
`apollo-router/src/configuration/metrics.rs`, because one cannot cover both
paths: `apollo.router.config.connectors_source_aware` for the config path, and
`apollo.router.supergraph.connectors_source_aware`, carrying an `enabled_by`
attribute, for the schema-derived path the config mechanism cannot see.

**Byte-identical when off, verified rather than asserted.** The identity channel
is an `Option`, serde-skipped and absent from `Display`, so existing plans
serialize identically. Every slice re-ran the flag-off comparison as an
acceptance criterion.

**Self-gating instead of runtime conditionals in shared code.** The riskiest
edits are in `graph_path.rs`, core traversal that runs for every federation
user, connectors or not. Those relaxations are not gated on the config flag;
they are gated on the `connector_boundary_copy` marker, which only the
source-aware pass creates. Expansion graphs contain no marked nodes, so the new
paths are structurally unreachable rather than conditionally skipped.

**Conservative pass guards.** The graph pass transforms only when something is
genuinely prunable and a re-entry edge exists. Where a connector under-provides
and the landing type has no key, it preserves today's behavior rather than
introducing a new plan-time error.

**A correctness oracle instead of review by eye.** `check_plan`, the `plan-diff`
CLI (`apollo-federation/cli/src/plan_diff.rs`), and the fixture corpus make
"equivalent" a computed verdict.

**Executable repros, written before their fixes.** Each gap closed so far was
first added to the suite as an `#[ignore]`d test naming it, then un-ignored when
it went green. That is how the work was done rather than a standing guarantee:
there are no `#[ignore]`d placeholders for the unbuilt items below, so each
follow-on begins by writing its own fixture.

## What is not built

- **Source-aware cost model (row 6).** With connectors collapsed into one
  subgraph, the planner's boundary-crossing cost no longer prices per connector
  call. Real source-aware cost has to price per source-entering edge. Not needed
  for a working thread, needed for *good* plans. See
  `history/SOURCE_AWARE_COST_DIVERGENCE.md` for the fixture that would
  demonstrate it: a shareable field resolvable from both a connector already in
  use and a separate GraphQL backend, where expansion picks the second backend
  because batching makes it look cheap, and a source-entering cost model keeps
  the work on the backend already open. The honest headline there is "fewer
  distinct backends touched", not "fewer calls"; genuine call-count collapse
  additionally needs a batch-aware connector cost model.
- **Intra-connectors `$this` dependencies.** A connector reading `$this.<field>`
  needs that sibling field fetched first. Source-aware plans over the raw graph,
  where nothing encodes the read, so it satisfies only the declared `@requires`
  and never fetches the field at all.
  `apply_connector_parent_conditions` is the graph half of a fix and does make
  the planner fetch the reads, but it is **deliberately not wired in**: with it
  applied `correctness::check_plan` rejects the plan, and the rejection is right,
  because `@requires` names external fields and a sibling field of the same
  subgraph is not external. Federation has no way to say "this field needs that
  sibling field of the same subgraph," so the raw supergraph cannot justify the
  fetch the connector actually needs. Where an intra-connectors field dependency
  should be represented is an open design question, and
  `plans_this_variable_reads_as_fetch_inputs` records the gap by asking the
  connector what it needs and the plan what it supplies, so it carries no
  snapshot to rot and starts passing on its own if the gap closes.
- **Composition-side satisfiability.** The measured 2x to 9x blowup is a
  compose-time cost, and it lives in the composer rather than the router.
  Nothing here addresses it.
- **Nested output shapes**, meaning recursion into object-typed fields. Not to be
  confused with nested output *positions*, which is built and is in "Status"
  above. `connector_provided_fields` reads only the top level of the output
  shape, so a connector returning `address { city }` still lands on a shared
  `Address` node exposing `zip`. Same silent-null bug, one level down.
- **Observability parity (row 7).** Under one `connectors` subgraph, per
  connector spans and metrics may collapse to `service_name = "connectors"`
  unless reattached via the coordinate. Nobody has checked whether connector
  telemetry keys on coordinate or on service name. This is the only unsurveyed
  item, and silent capability loss is the worst failure mode for an opt-in flag.

**The honest headline:** the collapsed-graph approach has a real correctness
ceiling at row 3, which the current plan-time patches address case by case. The
uniform fix is typed source identity in the query graph, so that rows 2 through
4 are handled by the graph model instead of by patches at three different
layers. Everything here is flag-gated, so none of it is a regression risk to the
expansion path; it bounds how far the source-aware path can go.

## Follow-ons, in order

1. **No-key semantics.** Small, but decision-heavy. Where a connector
   under-provides and the type has no key, there is no follow-up fetch to issue,
   and today's silent null persists. The options are a plan-time error, keeping
   the null but emitting a diagnostic listing unreachable `(connector, field)`
   pairs, or both behind a strictness knob. Recommendation on record is the
   diagnostic first: one sitting, no behavior change, and it produces the
   evidence for whether erroring is ever safe to ship.
2. **Nested output shapes.** The real correctness frontier. Walk the output
   shape recursively alongside the graph, so an object-typed provided field
   points at a recursively restricted copy rather than the shared full node.
   Memoize per `(connector, type position, shape node)`, with a cycle guard for
   recursive types. Nested entities re-enter via keys; nested non-entities
   correctly become planner errors, which is why item 1 comes first.
3. **Type-level entity resolvers.** The graph pass already emits the `_entities`
   fetch, but stamping matches only `EntityResolver::Explicit`, so a `TypeBatch`
   or `TypeSingle` connector would leave the fetch unstamped and mis-dispatch.
   Mostly a stamping-match extension.
4. **Multi-connector variants on one field.** Pruning to the union of provided
   sets is safe but imprecise against single-coordinate dispatch. Per-variant
   boundary copies are the first real stepping stone toward typed source
   identity, and belong to the cost-model era.
5. **Interfaces and unions as connector output.** Structurally the same
   recursion as item 2, so build item 2 first. No fixture exercises this today;
   write the fixture before deciding it matters.

Full reasoning, including two explicit non-goals and why they are not unlocked
by the restrictive-provides pass, is in
`history/SOURCE_AWARE_RESTRICTIVE_PROVIDES_FOLLOW_ONS.md`.

## Working on it

**Federation side.** `query_plan/source_aware.rs` (entry point and the pass
wiring), `query_plan/connector_stamp.rs` (stamping, provided-fields,
fetch splitting), `query_graph/connect_graph.rs` (source-entering edges and the
restrictive-provides pass), `query_graph/graph_path.rs` (two of the three
self-gated traversal relaxations), `query_plan/query_planning_traversal.rs` (the
third — the "fully local selection" shortcut, gated on
`QueryGraph::nodes_reaching_connector_boundary_copy`),
`query_graph/build_query_graph.rs`
(`precompute_non_trivial_followup_edges`, refactored so the pass can re-run it).

All three relaxations are gated on `connector_boundary_copy`, so they are
unreachable in an expansion graph. Expect to find more: they exist because
planner optimizations assume **"same type ⇒ same fields reachable"**, and
boundary copies deliberately break that. Note the third is a different *category*
from the first two — those eat a path, while it skips path-building altogether —
so "no option disappeared" is not evidence that no assumption was tripped.

**Router side.** `spec/schema.rs` (the fork), `query_planner/query_planner_service.rs`
(planner construction and stamping), `plugins/connectors/query_plans.rs`
(coordinate resolution), `plugins/connectors/make_requests.rs` (request
building), `services/fetch_service.rs` and `services/connector_service.rs`
(dispatch).

**Verification recipe**, in the order that fails fastest:

1. Federation plan-shape test in `source_aware.rs`.
2. Router end-to-end repro in `plugins/connectors/tests/mod.rs`, using the
   expansion path as the oracle and asserting both response equality and
   dispatched-request equality — **but only where expansion is correct**; see
   "Where the two paths diverge, on purpose" above, and prefer a
   `tests/samples` case when the point is that the paths differ.
3. Full federation corpus plus the connectors suite, for flag-off safety.

```
cargo test -p apollo-federation
cargo test -p apollo-router --lib plugins::connectors
cargo test -p apollo-router --test samples --features snapshot connectors
cargo clippy -p apollo-federation -p apollo-router --all-targets
```

Then the three commands `cargo xtask lint` actually runs, because clippy alone
is **not** the lint gate and the `cargo doc` half has caught this branch twice:

```
cargo clippy --all --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-Dwarnings" cargo doc --all --no-deps
cargo fmt --all -- --check
```

Two silent-skip traps in the samples harness, both of which look like a passing
run: a sample with `"snapshot": true` is skipped entirely without
`--features snapshot`, and `Plan`/`Action` use `serde(deny_unknown_fields)`, so
one stray key in `plan.json` skips the whole sample with no message.

Diagnostic printers, which are not assertions and want `-- --nocapture`:
`distance_probe_raw_vs_expanded_graph`, `mirage_check_entity_queries_over_raw_graph`,
`steel_thread_root_field_end_to_end`, `raw_vs_expanded_plan_diff`,
`dump_raw_graph_entity_plan`.

Two pieces of code on this branch have **no production caller** and exist as a
record of an architecture that was explored and set aside: `ConnectFetchDescriptor`
(`apollo-federation/src/connectors/source_aware.rs`) and
`entities_from_source_aware` (`apollo-router/src/plugins/connectors/make_requests.rs`).
Both were built for a design in which the planner emits connector-specific fetch
nodes carrying a descriptor and no `_entities` operation. The discovery that
raw-graph plans already emit standard `_entities` operations made the cheaper
design viable and left these unwired. Do not read them as the live identity
mechanism; the live mechanism is the stamped coordinate.

## History

`history/` holds the eight design and handoff documents this README condenses.
They are kept for their detail, particularly the file-and-line survey work in
`PHASE0_HANDOFF.md` and the full corpus tables in `SOURCE_AWARE_DISTANCE.md`.

Read them with two warnings:

- **Their summaries are stale.** They were written incrementally over the life
  of the spike and their opening and closing sections were rarely revisited.
  `SOURCE_AWARE_DISTANCE.md` is the clearest case: its TL;DR predates the mirage
  check and states the opposite conclusion, and its closing verdict predates the
  router pipeline it describes as unbuilt. The middle sections are accurate.
- **Their commit hashes no longer resolve.** The branch has been rebased and its
  history squashed since they were written, so hashes cited in their evidence
  tables are not reachable from this branch. Where they name a test, a function
  or a file, those references are still good.
