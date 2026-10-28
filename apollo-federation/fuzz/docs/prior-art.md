# What this harness took from the apollo-rs prototype

An earlier prototype of the same work — a Rust `query_inclusion` checker in
`crates/apollo-static-analysis`, plus Lean differential fuzzing — was built in the `apollo-rs`
repository on the `static-analysis` branch. Those worktrees are gone; this records what was worth
keeping, so the conclusions do not have to be rediscovered.

## The finding that matters

From its `FUZZ_COVERAGE_PLAN.md`:

> Coverage-guided differential fuzzing of `query_inclusion` saturated twice while missing real
> bugs: three Rust-vs-Lean divergences in the entry gates (found by manual audit), the Boolean
> union case-split bug in apollo-federation (router#10024, found only after reading the fix), and
> the covariant type-case propagation bug (found by manual audit). The pattern: libFuzzer's
> coverage signal saturates on the *fixture generator*, whose hand-enumerated mutation families
> sample the checker's semantic space only where a human anticipated a shape. **Every bug so far
> lived in a shape nobody had enumerated.**

Five bugs, none found by the fuzzer. That is the strongest available evidence about what a green
campaign here is worth, and it applies directly: this harness's byte grammar is a hand-enumerated
generator with the same blind spot. A clean 500,000-comparison run says the shapes in the grammar
agree, and says nothing about the shapes outside it.

## What was adopted

**The metamorphic lane** (its Step 3, "verdict-aware rewrites instead of enumerated families").
Mutation families are closed; a rewrite system is open. `runners/metamorphic.rs` applies random
sequences of edits that leave both response shapes unchanged and requires the verdict to be
unchanged. Composing them reaches shapes no family enumerates, and it needs no oracle, so it runs
at Rust speed. Two rewrites reproduce known bug shapes by construction: `split-complementary`
emits the Boolean-union case-split shape behind router#10024, and `partition-by-runtime-type`
emits the covariant type-case shape.

**Categorizing unreached regions** (its Step 1). Its `FUZZ_COVERAGE_REPORT.md` sorts every
unreached region into: out of this lane's scope, a concrete generator gap, or justified
unreachable on validated inputs. The rejection-reason tally in `runners/differential.rs` is the
cheap version of the same discipline — a `Mismatch` variant that never appears is a generator gap
with a name.

**Grammar dimensions its 20 families covered and this one did not.** Variable-declaration
variants, structured arguments, and per-object field sets came from reading that catalogue. Its
remaining families that are still not reachable here are listed under "does not cover" in the
README.

## What was deliberately not adopted

**The `QI2` integer-profile oracle protocol.** The prototype shipped a 12-integer fixture profile
to Lean and found it could not describe rewrites or generated schemas, which is why its plan
proposes a `QI3` serialized-IR protocol as "the foundational piece". This harness sends raw bytes
decoded by the same grammar on both sides, which has the same ceiling: the metamorphic lane cannot
reach the Lean oracle, because the rewritten documents cannot be expressed as a byte string the
Lean decoder would produce. That is the known structural limit here, and serializing the IR is the
way past it.

**Its checker.** `query_compare` is a direct port of `QueryInclusion.includesBool`, not a port of
the prototype's independently-designed checker. The three entry-gate divergences its manual audit
found were all cases where that checker was *stricter* than the model — an extra
boolean-variable-support equality gate, a variable-definitions gate that constrained more than
shared names, and a fast path blind to contradictory cumulative clauses. A direct port cannot
acquire an extra gate, and the one analogous check here (shared variable declarations) is now
exercised 3,994 times per campaign against Lean.

## Still open

Ranked by what would move confidence most, with the cost of each.

### 1. Make the lanes a mutation kill signal — *partly done*

`cargo-mutants` runs over `query_compare` today with the crate's **unit tests** as the kill signal;
see the Status section of the README for the rate. That measures unit-test adequacy, not lane
adequacy, and the two differ: the first survivor found was `delete match arm (None, None)` in the
shared-declaration check, which no unit test covers but the differential lane hits thousands of
times per campaign.

Measuring the *lanes'* kill rate needs them reachable from a `cargo test` that cargo-mutants can
run, and cargo-mutants will not reach outside the workspace of the package it is invoked in. This
package is a separate workspace by design, so `src/properties.rs` (a bounded, oracle-free version
of every property) is not visible to a campaign rooted at the router workspace.

Two ways out, neither taken yet:

- move the generator behind a non-default `fuzz-model` feature of `apollo-federation`, so the
  property tests live in that crate and run in ordinary CI. One generator, no duplication, and the
  kill signal comes for free. Costs ~700 lines of generator inside the published crate, gated off.
- drive individual mutants by hand: `--shard i/N` selects one, and `--in-place` edits the real
  files, so a script could apply one mutant and run this package's tests. Fiddly and slow, but
  needs no restructuring.

### 2. A bug zoo

Re-introduce each of the five historical bugs behind a test-only flag and assert the appropriate
lane kills it:

1. a variable-definitions gate stricter than shared-name compatibility;
2. a boolean-variable-support equality gate;
3. a missing-response fast path blind to contradictory cumulative clauses;
4. the Boolean union case-split (router#10024, the `split-complementary` shape);
5. covariant type-case propagation (the `partition-by-runtime-type` shape).

Cheap, and it converts a documented list into an enforced one. Note that 1-3 are shapes a *direct*
port cannot acquire — they were extra gates in an independently-designed checker — so they test the
lanes rather than the checker.

### 3. Serialized-IR oracle

The structural limit. Both this harness and the prototype ship a *profile* to Lean (raw bytes here,
12 integers there) that both sides decode with their own copy of the same grammar. Consequences:

- the metamorphic lane cannot reach Lean, because a rewritten document is not expressible as a byte
  string the Lean decoder would produce. The open generator therefore has **no correctness oracle**
  — it only checks self-consistency, so a checker that is consistently wrong the same way before
  and after an edit is invisible to it;
- a varied schema dimension is out of reach for the same reason;
- the grammar exists twice and can drift. The schema digest guards the tables, not the decoder.

Shipping the schema and operation IR instead removes all three. Its plan calls this "the
foundational piece"; it needs `FromJson` for `Schema` and `Operation` on the Lean side, roughly 250
lines.

### 4. Open the schema dimension

Effectively blocked on 3. All five historical bugs were Boolean- or covariance-side, which the
prototype's plan attributes to those being the only richly-varied dimensions — so a fixed schema
predicts where the next miss lives rather than merely being an absence. Wanted: deeper interface
diamonds, union/interface overlaps beyond the current one, composite/non-composite switches across
implementors, and eventually generated schemas.

### 5. Structure-aware mutation

Have libFuzzer mutate the decoded IR rather than raw bytes, so its energy explores checker behavior
instead of decode space. Only worth doing after 3.
