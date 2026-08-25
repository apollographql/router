# Source-aware query planning: speaker sheet

Ten beats, ~27 min at the times below, with two cut paths at the end (one for
time, one for a mixed room).

This is the shareable copy: the stage material about the work, without the
speaker's private notes on handling particular people in the room.

Backing document: `dev-docs/source-aware/WALK_THROUGH.md` on
`benjamn/source-aware-query-planner`. Section numbers there match beat numbers
here, so you can answer a detail question by opening the file at the same number.

**The spine, if you only keep one sentence:** the planner's search was never the
hard part; the hard part is what the graph is able to say.

---

## 1. The comment (1 min)

*On screen:* `apollo-federation/src/connectors/mod.rs`, unmodified, from `dev`.

> Until we have a source-aware query planner, work with connectors will need to
> interface with standard query planning concepts...

*Say:* present tense, still there this morning. "This talk is what happened when
I tried to cash that check."

## 2. The trade (2 min)

One synthetic subgraph per `@connect`, then plan it as ordinary federation.
Composition, satisfiability, planning, execution all reused untouched.

Two things get encoded, and they are the whole story: the connector's
**identity** becomes the subgraph name, and its **boundary**, meaning which
fields it can serve, becomes that subgraph's field set. Both structural.

*Say clearly that it was a good trade.* The talk does not work if the room thinks
you are dunking on it.

## 3. The bill (2.5 min)

The headline is not a ratio, it is a scaling law. Expansion makes the number
of logical subgraphs proportional to the number of `@connect` directives, and
satisfiability grows superlinearly in that count — measured on Constellation
staging at roughly **N^2.5 in memory and N^3.5 in time**. Six connector
subgraphs became **1,398 synthetic ones**; the satisfiability check over that
expansion peaked at **~14 GB and ~260 seconds**, while every other
composition phase combined stays under 0.6 GB.

*If someone asks for the fixture numbers:* worst case is 8.8x edges / 21x key
edges — a shape-dependent multiple, not a coefficient. The count underneath
is what matters.

Then the runway. Across the fleet (May snapshot): median **1** connector,
p99 **54**, max **370**. Constellation is at **1,300+** — more than three
times the largest customer graph. The expansion trick bought time, and
dogfooding meant we hit the wall first, internally. At N^3.5, the head of
that distribution does not have to grow much before customers hit it too.

*And the consequence you can point at today:* the wall is already
operational. One composed graph cannot carry 1,300+ connectors, so
Constellation runs split across **separate routers**, orchestrated a level
up (MCP, for now).

Spend the last thirty seconds on the quieter cost. The cost model prices
**subgraph boundary crossings**, not backends. One HTTP backend split into
six synthetic subgraphs is priced as six independent systems, so the planner
cannot ask the only question that matters for a connector plan: how many
distinct external systems does this plan touch?

That converts the bloat from motivation into premise.

## 4. The false summit (3 min)

The plan was to write a new planner. It was not necessary. A roughly fifteen-line
seam ran the **existing** planner over the raw supergraph and produced a correct
plan on the first try.

Falsified deliberately: **39 operations, 39 `Equivalent`, 0 `Different`, 0
`Error`** — re-run live this morning, not quoted from a doc.

*Then disarm it yourself, before anyone asks:*

> Thirty-nine operations I wrote myself, against fixtures built to test the very
> thing I am removing. Several fixtures get a single query. The test calls itself
> a spike diagnostic, subset, and it is one. Call it a viability suite, not a
> corpus. It was enough to prove the approach is viable. It is nowhere near
> enough to prove it is safe.

*Land the generalization, which is the spine:* the planner's search was never the
hard part. The hard part is what the graph can say. Everything after this is a
consequence of that sentence.

## 5. The mirror pair (5 min, centre of gravity)

Same failure, both directions, one design decision. Both silent nulls. Neither
errors anywhere in the pipeline.

> **Status first, before you show it:** observed in development, fixed, pinned
> twice. Say the status *before* the failure at both halves of this beat, or the
> room spends the slide wondering whether your own flag is broken.

**Left: what collapsing the graph does.** One `User` node carries every field, so
the planner folds `username` into a fetch whose connector returns only `id name`.
Expansion was *physically incapable* of this.

Say "we watched it return null," not "it would have." The fix is
`restrict_connector_reachability`; it is pinned at plan-shape level by
`plans_entity_resolver_fetch_for_unprovided_field` and at the wire by
`source_aware_entity_resolver_connector_gap`, which runs both paths against mocks
and compares dispatched requests.

**Right: what expansion does.**

```graphql
Query.users: [User]
  @connect(selection: "id manager { id name } reports { id }")
```

| query | expansion | source-aware |
|---|---|---|
| `{ users { manager { name } } }` | `"Ada"` | `"Ada"` |
| `{ users { reports { name } } }` | **`null`** | `"Grace"` |

> Status, again stated first: live in production today, and pinned wrong on
> purpose.

Phrase that carefully: *the sample's expected output records expansion's `null`
deliberately, so that if expansion is ever fixed the test fails loudly instead of
being quietly updated.* Discipline, not indictment.

**Run it live.** One flag flip, both paths in one test via `ReloadConfiguration`.

If you have ten spare seconds: the fixture cannot pass for the wrong reason.
`/people/9` returns `"Ada (via resolver)"`, not `"Ada"`, so plain `"Ada"` proves
the restriction did not manufacture a needless entity fetch. `"Grace"` appears
nowhere in `/users`, so it cannot be an over-merge artifact.

**Prevalence, and this is where you answer "your sample is hand-built" before
anyone says it:** this shape sits in **21 of 5,422 real schema families**. Dell's
`Mutation.validateServiceablePart` is `manager`/`reports` modulo names, with
`ServiceableAssetPart` wide at `part.part` and narrow at `part.substituteParts`.
Say the honest clause out loud: *the shape is in the wild, not observed nulls.*

*The line you want repeated at lunch:* expansion explodes too much for
performance and not enough for correctness, and it is the same decision both
times.

## 6. The question (2 min)

Put the hard question on the screen yourself rather than waiting for it from
the floor, and credit whoever asked it first:

> The representation is *allowed*. How do we know it is not *buggy*?

Everything before this beat says "it works." Everything after says why that
sentence is not worth much on its own. Beat 5 has just earned the question,
because the room has seen the boundary be the whole game.

If it turns into a conversation mid-beat, bank it rather than spend it: the next
three beats are the answer, so offer to come back at the end if they are not.

## 7. What we found when we looked (90 sec, deliberately short)

Umbrella first: **the planner caches conclusions about a node, and boundary
copies make the cache lie.** At least three optimizations assume same type means
same fields reachable. All three were found by hitting them, not predicting them.

One example is enough: detour elimination found a "direct path" ending on the
pruned copy itself and discarded the re-entry, because it compares nodes by type.

Then own the exception: one of the three is a different category entirely. The
first two eat a path; the third skips path-building altogether. So "no plan
option disappeared" is not evidence that no assumption was tripped. Even the
exceptions fail silently.

## 8. Where the raw graph cannot say it (4 min, the most original claim)

Connector mapping expressions read sibling fields of their own parent type
through `$this`. Real data dependency: the fetch cannot dispatch until those
resolve.

Expansion learns it *by accident*. `Connector::resolvable_key` derives the
synthetic `@key` from `variable_references()`, which is why `simple.graphql`'s
`User.d` expands to key `"b c"` while declaring only `requires "c"`.

Source-aware plans over the raw graph, satisfies the declared requires, and never
fetches `b` at all.

The fix half exists and is **deliberately not wired in**.
`apply_connector_parent_conditions` does make the planner fetch the reads, but
then `check_plan` rejects the plan, and the rejection is right: `@requires` names
external fields, and a sibling field of the same subgraph is not external.

> **Federation has no way to say "this field needs that sibling field of the same
> subgraph."**

*Land it:* expansion's explosion did not only cost us. Splitting each connector
into its own subgraph made an intra-connector dependency cross-subgraph, at which
point federation could express it as a key. **The explosion bought vocabulary.**

*And the consequence, which is the better ending for this beat:* the end state is
not "remove expansion." It is **teach the graph to say what expansion says**.

Corpus support: 13 schema families read a `$this` field that sits outside an
existing `@key`. `Asset.partsPath` reading `manufacturer` and `serialNumber`;
`Property.socialCarousel` reading `spiritCodeComposite`; `Partner.readyInvoices`
reading `sfdcId`.

## 9. What is actually being claimed (3 min)

Not "it is not buggy." Three narrower things:

**Containment is structural, not conditional.** The traversal relaxations gate on
`connector_boundary_copy`, a marker only the source-aware pass sets. In an
expansion graph the copy set is empty by construction, so the new paths are
unreachable rather than skipped by a conditional. A federation user with no
connectors cannot reach that code.

**Verdicts are computed, not eyeballed** *, and here is the limit of that.*
`check_plan` is one oracle and a contested one: there are operations where the
planner emits a plan its own checker rejects and nothing adjudicates. So three
evidence classes deliberately do not depend on it: wire-level e2e comparing
dispatched requests, corpus prevalence which does no planning at all, and the
self-reporting invariants below. Worth adding: the one time the checker and the
planner disagreed on this branch, in beat 8, the checker was right and I deferred
to it.

**Unproven invariants report themselves.** At the unguarded `can_rebase_on`, the
question is answered from the parent's *subgraph schema*, which a boundary copy
invalidates. No known operation reaches the hazardous combination, so rather than
change plan shape on a hazard with no reproduction, it warns. Its comment: an
unproven invariant that reports itself is worth more than one nobody would notice
breaking.

*The reason all three are detection rather than confidence:* **the failure mode
is silence.** A path quietly disappears, or a field comes back null. Nothing
errors.

**Corpus, as measurement rather than posture.** 7,375 graphs in the fleet corpus, 5,422
distinct schema families, about five seconds to run. 21 families contain the
over-merge shape; 13 read a non-key `$this`; 514 under-provide against a keyless
type. Two caveats, said out loud: these are graphs that *contain the shape*, not
graphs observed returning null, and the first two are **lower bounds** because
the reader refuses to half-parse. Offer to run it live.

## 10. State, and the close (2 min)

**Two triggers, not one.** `experimental_connectors_source_aware` is off by
default, and separately a connect **v0.5** supergraph is never expanded, so it is
always planned source-aware regardless of config. Say that explicitly. "Off by
default" alone is now false, and anyone who read the branch README before this
week has the older model.

**The ceiling, honestly.** Rows 2 through 4 of the replacement table are patches
at three different layers. The uniform fix is **typed source identity in the
query graph**, so the boundary lives in the model instead of in three patches. It
pays both bills: beat 7's, by making the planner unable to forget the boundary,
and beat 8's, by letting the graph express an intra-source dependency without
splitting the source.

Follow-ons in order: no-key semantics, nested output shapes (recursion into
object-typed fields, distinct from the nested *positions* work already done),
type-level entity resolvers.

*Close, paying off beat 1:* "The comment says *until we have a source-aware query
planner*. We now know roughly what that sentence costs."

---

## Cut path A: short slot

For ten minutes keep **1, 4, 5, 6, 8**, and one line of 10. The mirror pair and
the vocabulary finding are the two things nobody else can tell them.

Drop in this order: 3, then 7, then 9 down to the single "the failure mode is
silence" line.

## Cut path B: mixed room

If the room is not planner-literate, **collapse 7, 8 and 9 into one slide** you
can switch to live, titled roughly *why "it passed my tests" is not a safety
claim, and how failures report themselves*. Keep 8's punchline sentence inside
it, because it survives without the machinery.

Designed as a slide-skip, not an on-stage re-derivation.

## Demo rehearsal, once, before you go

The samples harness has two silent-skip traps that both look exactly like a
passing run: a sample with `"snapshot": true` is skipped entirely without
`--features snapshot`, and `Plan`/`Action` use `serde(deny_unknown_fields)`, so
one stray key in `plan.json` skips the whole sample with no message.

```
cargo test -p apollo-router --test samples --features snapshot connectors
```

**Verified passing this morning**: `1 passed; 0 failed` in 6.88s.
The log shows the divergence happening: the expansion phase fetches only
`/users`, then after the config reload the source-aware phase adds `/people/2`
and `/people/9`, which are the entity-resolver fetches that produce `"Grace"`.

Corpus probe, if you want it live:

```
cd ~/dev/connectors-corpus-may-2026
./extractor/target/release/probe graphs-20260506 >/dev/null
```

## Provenance of every number on these slides

All of them were computed today, against the branch tip:

- **39/39 Equivalent**: re-run live this
  morning via `cargo test -p apollo-federation raw_vs_expanded_plan_diff`. The
  README previously said 32 operations; the live count is 39, so the older
  figure understated it.
- **The 8.8x / 21x graph figures**: re-run this morning via
  `distance_probe_raw_vs_expanded_graph`. Now backup-only — beat 3 leads with
  the scaling law and keeps these for the Q&A.
- **The expansion wall** (6 → 1,398 subgraphs; ~14 GB / ~260 s; ~N^2.5 memory
  / N^3.5 time): measured on Constellation staging during the
  satisfiability-collapse work (PR #9663 era), **not** re-run today. If anyone
  presses, the honest answer is the exponents come from a subset sweep of that
  one graph.
- **Fleet connector distribution** (median 1 / p99 54 / max 370 vs our
  1,300+): measured this morning by `extractor/src/bin/probe.rs` over the May
  2026 snapshot. The snapshot is three and a half months old; there is no
  growth-rate measurement, so "nobody is near it yet" is a fact about May.
- **Corpus prevalence** (5,422 families; 21 / 13 / 514): computed today by
  `extractor/src/bin/probe.rs`, about five seconds, reproducible on stage.

- **The live demo** (`sibling-position-over-merge`): run today with
  `--features snapshot`, passes in 6.88s.

Still not re-confirmed since an older run, and not load-bearing for any slide:
the full connectors suite and the whole `apollo-federation` suite.
