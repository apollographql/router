# Op-gen Phase A sweep — one new divergence class found

After Phase P (added three post-passes to `op_gen.rs::decorate_operation`,
on top of the existing `@skip` / `@include` / `@defer` decorator):

1. **`__typename` sprinkle**: at any non-empty selection set without a
   pre-existing `__typename` field, append one. Default chance 50/256.
2. **alias-with-skip duplication**: leaf scalar fields get a sibling
   alias-duplicate with `@skip(if: $v)` on the duplicate. The clone
   strips any pre-existing `@skip`/`@include` to honour the
   non-repeatable rule. Default chance 60/256.
3. **fragment extraction**: eligible inline fragments (those with a
   type condition and a non-empty body) are lifted into named
   `FragmentDefinition`s; the original site becomes a `FragmentSpread`
   that inherits the inline fragment's directives. Default chance
   80/256.

apollo-smith never produces named fragment definitions and produces
shallow alias / `__typename` distributions — Phase A widens those
shapes without modifying smith.

## Sweeps

| BASE -> 2.13.1                                                   | Iter / op  | Divergences |
|------------------------------------------------------------------|------------|-------------|
| `=2.5.0`, no defer (seed 17)                                     | 1000       | **0**       |
| `=2.5.0`, --enable-defer (seeds 17, 42, 1234, 99 — 4000 ops)     | 4 × 1000   | **0**       |
| `=2.0.0`, no defer (seed 17)                                     | 1000       | 74          |
| `=2.0.0`, --enable-defer (seed 17)                               | 1000       | 551         |

After fixing two op-gen validation issues (non-repeatable `@skip` on
alias duplicates; alias-name collisions between selection sets — moved
to a global counter), `ops_skipped` is **0** across all sweeps.

## Findings

### 2.5.0 → 2.13.1: still byte-identical

Across **5000 ops** with the richest schema/op surface this harness
has ever produced — single + compound `@key`, multi-field
`@requires`, `@override` (incl. progressive), `@interfaceObject`,
`@provides`, inter-entity references, AND now `__typename` sprinkling,
alias-with-skip duplication, named fragment definitions, and `@defer`
— zero plan divergences. The 2.5.x → 2.13.x window remains converged.

### 2.0.0 → 2.13.1, no defer: **new Class E** surfaced

Categorization of the 74 divergences:

| Pattern | Count |
|---|---|
| PR #7580 only (`... on Query`) | 22 |
| PR #7580 + Condition (FED-505) | 51 |
| **Class E: selection reordering for aliased duplicates** | **1** |

#### Class E

When a selection set contains aliased duplicates of the same field,
HEAD reorders them in the emitted subgraph operation document; BASE
preserves declaration order:

```diff
-"qT1 { ... id @skip(if: $__v0) aS4: id @skip(if: $__v0) aS3: id @skip(if: $__v1) }",
+"qT1 { ... aS3: id @skip(if: $__v1) id @skip(if: $__v0) aS4: id @skip(if: $__v0) }",
```

Looks like canonicalization (deterministic sort) added in HEAD that
2.0.0 didn't do. Probably benign correctness-wise — both orderings are
valid GraphQL — but a real plan-emission divergence that was invisible
before alias-with-skip duplication existed in op-gen.

Surface activation in the 74 divergences:
- 70 / 74 diffs explicitly mention `aS<n>` (alias-skip): exercised but
  the diffs are dominated by the unrelated PR #7580 / FED-505 patterns.
- 72 / 74 diffs mention `__typename`: heavily exercised.
- 0 / 74 diffs mention `FragN`: the planner inlines named fragments
  before emitting subgraph ops, so they don't appear in the plan
  output. Fragment definitions are still being emitted in the source
  ops (sanity-checked separately via a one-shot run); the planner is
  just resolving them away.

### 2.0.0 → 2.13.1, --enable-defer: same 4 known classes

551 divergences against 2.0.0 + defer + Phase A. All fall into the
known A/B/C/D categories (PR #7580 / FED-505 / defer-C `sub_selection`
/ defer-D `Field` representation) — the alias-reordering Class E
either gets masked by the more substantial defer-related differences
in those plans, or doesn't co-occur with defer in the same op shape.

## Combined picture (Phases I, J, K, L, M, N, O, P — 33000+ ops swept)

> Across the schema/operation surface this generator now covers —
> compound `@key`, multi-field `@requires`, `@override` (incl.
> progressive), `@external`, `@shareable`, interfaces,
> `@interfaceObject`, `@provides`, inter-entity references with
> multi-hop traversal, `@defer`, `__typename` sprinkling, alias-
> with-skip duplication, AND named fragment definitions — the planner
> from 2.5.0 forward is byte-identical to 2.13.1 on every generated
> case.
>
> Total divergence classes between 2.0.0 → 2.13.1:
>   A. PR #7580 (extraneous `... on Query` inline fragment)
>   B. FED-505 / missing Condition node
>   C. Defer wire format: `Defer.primary.sub_selection` field added
>   D. Defer Field representation: string-with-directives → `{response_key}`
>   **E. Selection reordering for aliased duplicates** ← NEW
>
> All five fixed in (2.1.3, 2.5.0].

## Curated artifacts

- `class_E_alias_reordering.txt` — the standalone Class E divergence:
  no PR #7580, no Condition node, just selection reordering of
  alias-with-skip duplicates between the two planner versions.
- `phaseA_with_fragment_definition.txt` — a representative op showing
  `fragment FragN on T...` definitions in the source op (the planner
  inlines them, so the diff is in unrelated PR #7580 territory).
- `cross_version_2.0.0_phaseA_defer/phaseA_with_defer.txt` — Phase A
  shapes interacting with the defer-C `sub_selection` divergence.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_phaseA

cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 --enable-defer \
    --regressions-dir tests/regressions/cross_version_2.0.0_phaseA_defer
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
