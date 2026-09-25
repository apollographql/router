# Differential fuzzing against the Lean models

Two lanes live here, sharing one transport and one architecture:

- **query inclusion** — `query_compare` against `QueryInclusion.includesBool`, plus a 3-way
  benchmark. Everything below the "Query inclusion" heading is about this lane.
- **query plan checking** — `correctness::check_plan` and `correctness::legacy::check_plan`
  against `Federation.checkQueryPlan`. See "Query plan checking" at the end.

# Query inclusion: differential fuzzing and 3-way benchmark

This package checks `apollo_federation::correctness::query_compare` — a Rust port of
`QueryInclusion.includesBool` from the [graphql-lean](https://github.com/duckki/graphql-lean)
formalization — against the Lean model it was ported from, and benchmarks it against both the
model and the existing response-shape checker.

It follows the architecture established in
[`duckki/graphql-static-analysis-rs`](https://github.com/duckki/graphql-static-analysis-rs):
a fixed schema baked into both sides, a bounded byte grammar implemented twice, and a persistent
native Lean binary rather than a serialization format.

The package is its own Cargo workspace. It is not built by the router workspace, because the
differential lane needs a Lean checkout.

## Why there is no serialization format

Both sides decode the *same bytes* with their own implementation of the same grammar
(`src/model.rs` and `lean/QueryInclusionOracle.lean`). Nothing is parsed from one side to the
other, so there is no encoder to get wrong and no reader to debug. The cost is that the schema and
the grammar exist twice and can drift apart. That is what the `schema` request guards: the oracle
renders its own copy of the schema tables and every run refuses to proceed unless it matches
[`model::schema_digest`].

See [prior-art.md](docs/prior-art.md) for what this took from an earlier prototype of the same work,
including the reason to distrust a green campaign from the byte grammar alone.

## Lanes

| lane | implementation | role |
| --- | --- | --- |
| `lean` | `QueryInclusion.includesBool` | the model, carrying soundness and completeness theorems |
| `query_compare` | `query_compare::includes` | the port under test |
| `response_shape` | `correctness::compare_operations` | an independent algorithm for the same predicate |

`lean` against `query_compare` is a strict equality check — the port claims to compute exactly what
the model computes, so any disagreement is a defect. `response_shape` is advisory: it decides the
same predicate by a different route and the two are not claimed to be equally complete, so its
divergences are counted and reported rather than failed on.

Comparisons the response-shape checker cannot be expected to match are counted separately as
*scope gaps* rather than divergences. It does not model variable declarations at all, so when the
only reason for rejection is a shared declaration that differs, disagreement is guaranteed and
says nothing.

**The two APIs run in opposite directions.** `includes(left, right)` asks whether `right`'s
response is contained in `left`'s; `compare_operations(this, other)` asks whether `this` is a
*subset* of `other`. Passing the same argument order to both compares two different questions.
Getting this wrong was worth 374 spurious divergences per 2,000 cases before it was caught.

## Build the Lean oracle

```sh
scripts/build-oracle.sh /path/to/graphql-lean
```

The adapter is compiled and linked against the built model, and the Lean revision is recorded in
`target/query-inclusion-lean-oracle.model-commit`. The runner rejects a revision other than
`LEAN_MODEL_COMMIT` in `src/lean_oracle.rs`; set `QUERY_INCLUSION_ALLOW_STALE_LEAN_ORACLE=1` to
compare against another revision deliberately.

## Deterministic checks

```sh
# the full matrix: every combination of 8 byte values over the first 5 positions
cargo run --release --example differential -- \
  --lean-oracle target/query-inclusion-lean-oracle --exhaustive

# a seeded campaign, biased toward related operation pairs
cargo run --release --example differential -- \
  --lean-oracle target/query-inclusion-lean-oracle --seed 20260915 --cases 30000

# replay one input
cargo run --release --example differential -- \
  --lean-oracle target/query-inclusion-lean-oracle --input-hex 0a0b0c
```

Every disagreement prints a replayable `--input-hex`, both rendered operations, and the
`query_compare` explanation. Without `--lean-oracle` the runner still compares the two Rust
implementations and reports how often the grammar produced a document GraphQL rejects.

`--dump-seeds corpus/differential` retains one input per semantically distinct outcome, chosen by
observed behavior rather than written by hand. Inclusion is not symmetric, so the four
(forward, backward) combinations are the categories that matter: equivalent, each strict
containment, and unrelated.

Three guards keep a green run from being vacuous:

- the runner fails if every comparison answered the same way, since three lanes agreeing on a
  constant proves nothing;
- `--dump-seeds` reports which outcome categories a campaign never reached. The exhaustive matrix
  reaches only *equivalent* and *unrelated*: its five varying bytes make the two operations decode
  either near-identically or completely differently. Strict containment — the case an inclusion
  checker exists for — is reached only by the seeded campaign, which builds inputs whose halves
  decode to the same operation and then perturbs one byte. A matrix-only run would look green
  while never testing the interesting question;
- `QUERY_INCLUSION_SENTINEL=1` inverts every `query_compare` verdict, and the run must then fail.
  This proves a mismatch travels through the decoder, the oracle transport, the comparison, and
  the report — "no mismatches found" and "no detection wired up" are otherwise indistinguishable.

## Coverage-guided campaign

Run from the `apollo-federation` crate root, so `cargo fuzz` finds this package. libFuzzer writes
discoveries to its *first* corpus directory, so pass an ignored campaign directory first and the
small reviewable corpus second:

```sh
cd ..
mkdir -p fuzz/target/differential-campaign
QUERY_INCLUSION_LEAN_ORACLE="$PWD/fuzz/target/query-inclusion-lean-oracle" \
  cargo +nightly fuzz run differential \
  fuzz/target/differential-campaign fuzz/corpus/differential \
  -- -max_total_time=120 -max_len=64
```

`-max_len=64` matches the cap in the target: beyond the grammar's nesting budget the extra bytes
are unreachable, and letting libFuzzer grow inputs past it only wastes mutations.

## Property tests

`src/properties.rs` runs bounded versions of every oracle-free property as ordinary `cargo test`:
verdict invariance under rewrites, reflexivity, monotonicity, transitivity, and agreement between
the two Rust checkers. Seconds, not minutes — the runners below are the campaign versions.

## Metamorphic lane

The byte grammar is a *closed* generator: it produces the shapes someone thought to encode. The
prototype this harness learned from saturated its coverage-guided campaign twice while five real
bugs went unfound, every one of them in a shape nobody had enumerated.

`runners/metamorphic.rs` is the open counterpart. It takes a case and applies random sequences of
edits that leave both response shapes unchanged — wrap in `@include(if: true)`, wrap in the
parent's own type condition, duplicate, split into complementary `@skip`/`@include` branches,
partition over the parent's runtime types, reverse sibling order, permute the fields of an
input-object argument — and requires the verdict to be
identical. Composing them reaches shapes no family enumerates. It also checks three properties that hold independently
of any rewrite:

- **reflexivity** — every operation includes itself;
- **monotonicity** — growing the left operation and shrinking the right cannot break a holding
  inclusion;
- **transitivity** — if A includes B and B includes C then A includes C.

These catch a different class than verdict invariance: a checker can be perfectly stable under
meaning-preserving edits and still violate all three.

No oracle is involved, so it runs at full Rust speed and needs no Lean checkout:

```sh
cargo run --release --example metamorphic -- --cases 150000 --steps 6
cargo run --release --example metamorphic -- --input-hex 4c5e --rewrite-seed 645
```

Two of the rewrites reproduce known bug shapes by construction: `split-complementary` emits the
Boolean-union case-split shape behind router#10024, and `partition-by-runtime-type` emits the
covariant type-case shape.

The lane's structural limit is that it cannot reach the Lean oracle — a rewritten document is not
expressible as a byte string the Lean decoder would produce. Closing that needs a serialized-IR
protocol; see [prior-art.md](docs/prior-art.md).

## Benchmark

```sh
cargo run --release --example benchmark -- --lean-oracle target/query-inclusion-lean-oracle
```

The Rust lanes are timed in-process; the Lean lane is timed *inside* the oracle so the line
protocol is not charged to it. A per-call floor is reported separately, because `includes` builds
a possible-type index over the whole schema before it looks at either operation — see
`QueryComparator` for the reusable form.

## What the grammar reaches

`--dump-seeds` reports outcome categories; the runner also tallies which `Mismatch` variant each
rejection produced. A variant that never appears is a decision path the campaign has never tested,
however many executions it ran. Over 80,000 comparisons of the seeded campaign:

| reason | count | |
| --- | --- | --- |
| `MissingResponseName` | 31,711 | |
| `FieldNameMismatch` | 22,049 | |
| `VariableDeclarationMismatch` | 3,994 | |
| `FieldArgumentsMismatch` | 933 | |
| `RootTypeMismatch` | 0 | unreachable: one query root |
| `UndefinedField` | 0 | unreachable: invalid documents are discarded |
| `UnsupportedDirective` | 0 | unreachable: the grammar emits only `@skip`/`@include` |
| `UnsupportedFragmentSpread` | 0 | unreachable: the grammar emits no spreads |
| `Internal` | 0 | should never fire |

The exhaustive matrix reaches only `MissingResponseName` and `FieldNameMismatch`: it varies six
bytes, and even spread across the input those cannot drive every path. Breadth over structural
combinations is its job; the seeded campaign supplies depth.

## Status

Checked on 2026-09-15 against Lean commit `06a5d04d6c00b875d7da9c1c4f1c148b32191f0d`:

- the exhaustive matrix, 262,144 inputs and 524,288 comparisons, agrees;
- a 40,000-input seeded campaign, 80,000 comparisons, agrees, and reaches all four outcome
  categories;
- a 120-second coverage-guided campaign completed 85,669 executions with no failures;
- `response_shape` agrees on every comparison in all three;
- a 300,000-comparison metamorphic campaign (150,000 cases, six composed rewrites each) reports
  no verdict change and no reflexivity failure, in 20 seconds;
- `cargo-mutants` over the checker: 113 caught, 64 missed, 29 unviable — 63.8% for the crate's unit
  tests, with roughly 35 of the survivors unkillable by construction. See
  [mutation-report.md](docs/mutation-report.md);
- the sentinel is detected;
- the grammar produces no invalid documents (0% discarded);
- LLVM source coverage of the module under test, from the two deterministic campaigns:
  `mod.rs` 92.4% regions / 94.1% lines, `conditions.rs` 85.7% / 87.9%, `schema_view.rs` 82.8% /
  92.1%. `error.rs` is 20.6% / 29.0% here because a campaign only reads verdicts and never
  renders an error; the unit tests cover it at 70.3% / 75.6%.

The coverage-guided campaign found one defect on its first input, in the harness rather than in
either implementation: the empty byte string makes the request `includes` with an empty argument,
and trailing whitespace is stripped from the line before dispatch, so the oracle's
`startsWith "includes "` no longer matched and it answered `unknown request`. The oracle now
splits the command word from its argument instead of matching a prefix with a trailing space.
`corpus/differential/empty-input-minimal-operations` retains that input.

These are bounded checks over an encoded input space, not a proof of equivalence.

The grammar covers three `@skip`/`@include` variables and constant conditions, inline fragments
with overlapping type conditions, response names bound to differing resolver calls, multi-argument fields
with permuted argument order, optional arguments, and Boolean, enum, list and input-object
argument values, a covariant field return, a union
field return (whose selection set must consist of inline fragments), object types with fields the
interface does not declare, variables in argument position over three types
(`Boolean!` for directives, `Int!` and `String` for arguments) with five, three and two declaration
variants respectively, and nesting up to the budget in `model.rs`.

It does not cover named fragments, custom directives, mutation or subscription roots, nested
variables inside list or input-object argument values, or list-depth variation. The schema itself
is fixed:
only the operation pair varies, so schema-shaped reasoning is exercised only through the one
schema both sides hold.


# Query plan checking

Checks `apollo_federation::correctness::check_plan` — a Rust port of `Federation.checkQueryPlan`
from [apollo-graphql-lean](https://github.com/duckki/apollo-graphql-lean) — against the model, and
reports what the legacy response-shape checker decides beside it.

| lane | implementation | role |
| --- | --- | --- |
| `lean` | `Federation.checkQueryPlan` | the model, carrying `checkQueryPlan_correct` |
| `query_plan_check` | `correctness::check_plan` | the port under test |
| `legacy` | `correctness::legacy::check_plan` | an independent algorithm for the same question |

`lean` against `query_plan_check` is strict equality. `legacy` is advisory, and is *known* not to
ask two of the things the model asks — that every entity case is covered by some `requires` entry,
and that contextual data is available — so its divergences are counted, not failed on.

## The fixture

One composed supergraph, checked in at `fixtures/supergraph.graphql` and never generated: the
grammar varies the operation and the plan, not the schema. It is small enough to transcribe into
the Lean oracle by hand and still exercises what the checker decides — an interface root under a
list, two entity types with `@key(fields: "id")` and a `@requires` field each, and a third
implementation with no key at all.

## Why a case is an operation plus a perturbation

A plan drawn freely from bytes is almost never correct for the operation beside it, and a lane
whose every case is rejected exercises only the rejection paths. So a case is decoded in three
steps: an operation, the plan a miniature planner gives that operation, and one *named*
perturbation of that plan. The unperturbed plan is correct by construction.

Whether a *perturbed* plan is wrong is deliberately not decided by the generator: `DropKeyField`
breaks nothing when the operation selects `id` itself, and working that out is the checker's job.
`plan_model::must_be_accepted` therefore claims only that an unperturbed plan must be accepted;
the model is the oracle for the rest.

## Lanes that need no Lean

    cargo run --release --example plan_smoke

Walks the whole grammar against both Rust checkers, then plans every generated operation with the
*real* planner and checks the result. Those are the only plans that are correct by construction
rather than by the miniature planner agreeing with itself, so a rejection there is a false
positive in the checker under test.

## Build the Lean oracle

```sh
scripts/build-plan-oracle.sh /path/to/apollo-graphql-lean
```

That project declares no `lean_exe`, so unlike the inclusion oracle there is no ready-made linker
response file; the script links the static libraries instead. The Lean revision is recorded beside
the binary and `PLAN_MODEL_COMMIT` in `src/plan_oracle.rs` must match, unless
`QUERY_PLAN_ALLOW_STALE_LEAN_ORACLE=1` is set.

    QUERY_PLAN_LEAN_ORACLE=target/query-plan-checker-lean-oracle \
        cargo run --release --example plan_differential -- 20000

## Status

The lane is green against `26c8a96`: 12,960 cases exhaustively and 20,000 sampled, **zero
divergences** between the model and either Rust checker.

Its first campaign reported 1,114, all of which were a fixture bug in the oracle itself —
[oracle-fixture-drift.md](docs/oracle-fixture-drift.md) records what it was, why the schema digest did not catch it,
and what the digest now checks so that it would. `plan_disagreements` is the runner that distils a
campaign's divergences into the operand pairs behind them.

## Tracing a disagreement

The oracle answers more than `check`. `render <hex>` gives the operation and plan as *it* built
them, so a divergence can be blamed on the grammar rather than the checkers; `halves <hex>` splits
`checkQueryPlan` into its completeness and soundness bits; `fetchcheck <hex>` decides the soundness
of the entity fetch alone, with and without the condition it runs under; `guardsplit <hex>` runs
completeness with each side's guard removed in turn; and `fetched <hex>` renders the left operand
of the completeness test.

When that bottoms out at the inclusion relation, `plan_probe` takes the two rendered operands and
runs `query_compare::includes` on them, with no plan machinery left in the way:

    cargo run --release --example plan_probe -- '<left operation>' '<right operation>'

**Beware of hand-written byte strings.** The slot count decides how many slot bytes follow, so
changing a byte shifts what the later ones mean; a repro should come from the runner's own output.
