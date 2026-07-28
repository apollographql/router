# Source-aware cost divergence — the "plan gets cheaper" fixture

> **Purpose.** The Phase-1 north star is to *prove to non-technical folks that
> the query plan gets cheaper under source-awareness.* This doc contrives the
> query + schema shape where a **plausible future source-aware connectors cost
> model** produces a **strictly better plan** than today's expansion-blind cost
> model — and states, honestly, exactly how big that win is and what it depends
> on. Grounded in the real steel-thread schema
> (`apollo-router/src/plugins/connectors/testdata/steelthread.graphql`).

## What expansion's cost model is structurally blind to

Today the router **expands** a connector supergraph into one synthetic subgraph
per `@connect` directive. The steel-thread's single connector source —
`json` (`https://jsonplaceholder.typicode.com/`) — becomes ~6 synthetic
subgraphs (`Query.users`, `Query.me`, `Query.user`, `Query.posts`,
`User.nickname`, `User.d`). The planner's cost model prices **boundary crossings
between subgraphs**. It has **no representation of the fact that all six
synthetic subgraphs are one HTTP backend.**

So expansion's cost model cannot express the single most important physical
truth about a connector plan: *how many distinct external systems does this plan
actually touch, and how many times does it cross into a new one?* That blindness
is the entire opportunity.

## The divergence fixture

Take the one field in the steel-thread that is already resolvable **two ways**:
`User.c` — declared `external` in `CONNECTORS`, resolved by the `GRAPHQL`
subgraph (`https://localhost:4001`). `User.d @requires(c)` consumes it.

Contrive **one** additive change: also let the `json` source resolve `c`, via
the existing entity resolver (`Query.user`, `GET /users/{id}`, add `c` to its
`selection`). Now `c` is genuinely `@shareable` between **two sources**:
`json` (a connector we are *already calling*) and `graphql` (a separate
backend). This creates a real **cost choice** for the planner — which is the
prerequisite for any divergence.

**Query:**

```graphql
{ users { d } }        # d @requires(c); c now resolvable from json OR graphql
```

### Plan A — expansion's cost model (picks graphql for `c`)

```
wave 1:  GET  /users            (json)     → user ids
wave 2:  POST /graphql {_entities: c}       → c per user     [needs ids]
wave 3:  GET  /users/{c}        (json)     → d               [needs c]
```

- **2 distinct backends** (json, graphql), and the plan **leaves the json source
  and comes back** (json → graphql → json).
- Why expansion picks this: to its cost model, the json entity resolver and the
  graphql subgraph are just *two different subgraphs*. The graphql `_entities`
  fetch resolves `c` for **all** users in **one batched call**, whereas the json
  entity resolver is modeled as per-id `GET`s — so a fetch-count-based cost model
  actively **prefers graphql.** Expansion has no signal that graphql is a whole
  separate system while json is already open.

### Plan B — source-aware cost model (keeps `c` on json)

```
wave 1:  GET  /users            (json)     → user ids
wave 2:  GET  /users/{id}       (json)     → c               [needs ids]
wave 3:  GET  /users/{c}        (json)     → d               [needs c]
```

- **1 distinct backend** (json only). The plan never opens a connection to a
  second system.
- Why source-aware picks this: its cost prices **source-entering edges** — a term
  that costs *entering a source you are not already in*. Getting `c` from `json`
  crosses **zero** new source boundaries (we are already in `json`); getting it
  from `graphql` costs one full source-entry. So Plan B is strictly cheaper on
  the dimension that actually maps to latency, failure surface, auth, and rate
  limits.

## The two cost models, side by side

Let a plan's cost be `Σ_fetch ( source_entry_cost + response_cost )`.

| | Expansion cost model | Source-aware cost model |
|---|---|---|
| Unit of "source entry" | **per synthetic subgraph** hop | **per distinct backend** entered |
| Sees json's 6 subgraphs as | 6 independent subgraphs | **1 source** |
| `c` from json (already in json) | costs a full subgraph hop | costs **0** source-entries |
| `c` from graphql | costs a subgraph hop (and batches → looks *cheap*) | costs **1** source-entry (a new backend) |
| Picks for `c` | **graphql** (batched, looks cheapest) | **json** (no new source) |
| Backends touched | **2** | **1** |

## The non-technical headline

> **Same data, same result — but the smart plan gets what it needs from the API
> it's already talking to, instead of making a detour out to a second backend
> system and back. Two external systems touched → one.**

This is an honest A/B: the flag makes flag-off byte-identical, so this is the
*identical query* planned two ways — the most credible possible demo.

## Honest boundaries (so the demo doesn't over-claim)

1. **The pure, source-awareness-*only* win is "fewer distinct backends touched,"
   not "fewer total calls."** In the fixture above, Plan B still makes N per-id
   `GET /users/{id}` calls for `c`, whereas graphql resolves all N in one
   `_entities` batch. Source-aware touches **fewer backends** but not necessarily
   **fewer HTTP calls** — and naive call-counting can actually *favor* GraphQL,
   because GraphQL `_entities` batches and connectors are currently modeled as
   per-id fetches. Fewer-backends is the legible, defensible headline (latency
   risk, auth surface, rate-limit blast radius); "3 calls → 1 call" is **not**
   yet true from source-awareness alone.
2. **The bigger prize — genuine call-count collapse (N→1) — needs source-awareness
   PLUS a batch-capable connector cost model** that knows a source exposes a
   list/batch endpoint (`GET /users?ids=1,2,3`) and prices the collapse. That is
   a real, separable follow-on, and it's where the "3 calls → 1 call" headline
   becomes true. Source-awareness is the *precondition* (you can't batch across a
   source you can't recognize as one source), not the whole win.
3. **This requires the cost model that doesn't exist yet.** Post-2A, source-aware
   and expansion produce *equivalent* plans on the current fixtures — the
   divergence only appears once (a) the shareable-field choice above exists in a
   fixture and (b) the source-entering cost model is implemented. This doc is the
   **target** for that work, and de-risks it: a divergence shape provably exists.

## Suggested build path (when we pick this up)

1. Add the contrived `@shareable c` (json entity resolver `selection` gains `c`;
   `User.c` becomes resolvable in `CONNECTORS`) as a **new testdata schema**
   (don't mutate `steelthread.graphql`).
2. Assert the **expansion** plan for `{ users { d } }` routes `c` through
   `graphql` (2 backends) — the "before."
3. Implement source-entering cost (price distinct sources, not subgraphs) behind
   the source-aware flag; assert the plan keeps `c` on `json` (1 backend) — the
   "after."
4. Capture a **countable metric** — distinct backends entered / source-entering
   edges — so the before/after is a recorded diff, the demo artifact.
