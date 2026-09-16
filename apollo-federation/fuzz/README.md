# Query-inclusion differential fuzzing and 3-way benchmark

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

See [PRIOR_ART.md](PRIOR_ART.md) for what this took from an earlier prototype of the same work,
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
protocol; see PRIOR_ART.md.

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
  [MUTATION_REPORT.md](MUTATION_REPORT.md);
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
