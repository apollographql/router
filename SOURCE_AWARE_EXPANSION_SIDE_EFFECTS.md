# What connector expansion gave us for free — the accounting

> **Why this doc.** Today the router *expands* a connector supergraph into one
> synthetic subgraph per `@connect` directive. That explosion was doing many jobs
> at once, most of them **beneficial side-effects** the planner/executor got "for
> free" by treating connectors as ordinary subgraphs. Source-aware planning
> collapses connectors back into a single `connectors` subgraph — so **every one
> of those side-effects becomes something we must reconstruct deliberately.**
> This is the master checklist. Each row: what expansion did, whether
> source-aware handles it yet, and the evidence.
>
> Status legend: ✅ handled (with test) · ⚠️ partial/scoped · ❌ confirmed gap ·
> ❓ unverified (needs survey).

## The mechanism expansion used

For each `@connect`, expansion synthesized a **minimal subgraph** by walking the
connector's `selection` output shape — a type exposing *only* the fields that
connector actually returns — with a unique subgraph name, `@key`s for the entity
types it resolves, and `@join__field` home assignments. The planner then reasoned
over an ordinary multi-subgraph federated schema. Everything below falls out of
that one construction.

## The side-effects

### 1. Dispatch identity — ✅ handled
**Expansion:** each connector got a unique synthetic `service_name`; dispatch
keyed on it. **Source-aware:** replaced by a carried **coordinate**
(`FetchNode.connector`, B-1) stamped at plan time (B-2a) and resolved at dispatch
(B-3, `resolve_connector`). **Evidence:** `source_aware_dispatch_by_carried_coordinate`,
and every source-aware end-to-end test dispatches by coordinate.

### 2. Fetch decomposition / fan-out (parallelism) — ⚠️ partial
**Expansion:** distinct subgraphs → distinct fetch nodes → independent ones land
as parallel siblings; the executor's Parallel/Sequence machinery fans them out.
**Source-aware:** reconstructed in the plan by **2A** — `split_root_field_fetch`
splits a multi-connector `connectors` fetch into a `Parallel` of per-connector
fetches. **Evidence:** `source_aware_multi_connector_merged_fetch_end_to_end`.
**Scope gap:** only **root-field** merges; entity-field merges (shared
representations) and variable-bearing merges are deliberately left unsplit.

### 3. Per-connector field availability (output-shape boundaries) — ❌ confirmed gap
**Expansion:** the minimal subgraph exposed *only* the connector's provided
fields, so the planner physically could not route a field to a connector that
doesn't return it — and was forced to resolve such a field via a *separate*
connector (e.g. an entity resolver). **Source-aware:** collapsed, every
`User` field looks resolvable from any `User`-producing connector, so the planner
**over-merges**. **Evidence:** `{ users { username } }` plans as a single fetch
`connectors { users { username } }`; but the `Query.users` connector `selection`
is only `id name`, so `username` comes back **null** (repro
`source_aware_entity_resolver_connector_gap`). **The signal isn't lost:**
connectors expose `output_shape()` (derived from `selection`), so each connector's
provided top-level field set is computable — the basis for a fix. **Fix shape:**
detect a connectors fetch selecting fields its entry connector doesn't provide,
split those into a follow-up entity-resolver `_entities` fetch
(`Sequence[entry, Flatten[_entities]]`) — a cleaner, plan-level version of the
minimal-subgraph reconstruction expansion did structurally.

### 4. Entity-resolver identity + keys — ⚠️ partial
**Expansion:** synthesized `@key`s and entity-resolver join semantics, so the
planner knew *which* connector re-enters a type as an entity and *what key* it
takes. **Source-aware:** field-connector entity fetches (`User.d`) stamp
correctly; the entity-resolver class (`Query.user @connect(entity: true)`) is
addressed by the step-3 stamping change (match by entity **output type**) — but
that only bites once #3 forces a separate entity fetch to exist. Entity **keys**
themselves *are* preserved (they're on the raw schema: `User @join__type(key: "id")`).
**Evidence:** `stamps_connector_coordinates_over_raw_graph_plans` (User.d entity
fetch → `User.d`); step-3 stamping implemented but blocked on #3.

### 5. Input-dependency sequencing (`@requires` / entity inputs) — ✅ handled (so far)
**Expansion:** encoded each connector's input requirements so the planner ordered
dependent fetches. **Source-aware:** `@requires` lives on the raw schema, so the
planner sequences correctly. **Evidence:** step 1 —
`source_aware_entity_plus_requires_end_to_end`: `d @requires(c)` sequences
`GET /users` → graphql `_entities` (c) → `GET /users/{c}` (d) correctly. **Caveat:**
verified for `@requires`; connector-specific arg/`$this` inputs are exercised only
where they coincide with entity keys — worth watching as shapes widen.

### 6. Cost / planning granularity — ❌ (and this one is an *opportunity*, not just a gap)
**Expansion:** boundary-crossing cost priced per synthetic subgraph. This
side-effect is actually **harmful**: it prices one HTTP source as N subgraphs,
blind to the shared backend. **Source-aware** can do *better* — price
source-entering edges. **Evidence/target:** `SOURCE_AWARE_COST_DIVERGENCE.md`
(the "plan gets cheaper" demo). Not built; needs the source-entering cost model.

### 7. Observability granularity — ❓ needs survey
**Expansion:** per-connector spans/metrics keyed on the synthetic subgraph name,
for free. **Source-aware:** collapses to `service_name = "connectors"` unless
re-attached via the carried coordinate. **Status:** unsurveyed — does connector
telemetry key on coordinate/source or on service name today? (max_requests error
extensions already carry `connector.coordinate`, so it's partly there.)

## The through-line

Side-effects 1–5 are all the **same reconstruction problem** in different
clothes: expansion encoded per-connector structure (identity, boundaries, keys,
provided fields) into the federated schema; source-aware must carry that
structure some other way — as a stamped coordinate (done), a plan-time split
(2A, and the #3 fix), or eventually as typed source identity in the query graph
(**B-2b**, the production-grade version that would handle 2–4 uniformly instead
of case-by-case). #6 is the one place the side-effect was *harmful* and
source-awareness is a net **improvement** (the demo). #7 is a labeling task.

**Honest headline:** the collapsed-graph (B-2a) approach has a real **correctness
ceiling** at #3 — over-merging fields a connector can't serve — that the current
plan-time patches (2A + stamping) address only case-by-case. The uniform fix is
B-2b. Everything is flag-gated, so none of this is a regression risk to today's
expansion path; it bounds how far the *source-aware* path can go before B-2b.
