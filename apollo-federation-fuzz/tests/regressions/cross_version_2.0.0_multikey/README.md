# Multiple `@key` per entity sweep — negative result, closes COVERAGE_GAPS row 10

After Phase Q (each entity has a per-schema chance to get a *second*
independent `@key` directive on a fresh single non-null scalar field,
e.g. `@key(fields: "id") @key(fields: "sk0")`. Distinct from compound
keys: that adds extra fields to *one* `@key`; this adds an alternative
single-field `@key`. Every host of the entity uniformly declares both
keys and both key fields):

## Sweeps

| BASE -> 2.13.1                                                    | Iter / op  | Divergences |
|-------------------------------------------------------------------|------------|-------------|
| `=2.5.0`, no defer (seed 17)                                      | 1000       | **0**       |
| `=2.5.0`, --enable-defer (seeds 17, 42, 1234, 99 — 4000 ops)      | 4 × 1000   | **0**       |
| `=2.0.0`, no defer (seed 17)                                      | 1000       | 84          |
| `=2.0.0`, --enable-defer (seed 17)                                | 1000       | 552         |

Composition rate held at 100% (the secondary key field is uniformly
hosted, just like the primary's `id` and any compound key components).

## Findings

### 2.5.0 → 2.13.1: still byte-identical

5000 ops with multi-key + every other surface this generator now
exercises — zero plan divergences. The window remains converged.

### 2.0.0 → 2.13.1: same five divergence classes as Phase P

Categorisation of the no-defer 84 divergences:

| Pattern | Count |
|---|---|
| PR #7580 only (`... on Query`) | 34 |
| PR #7580 + Condition (FED-505) | 49 |
| Class E: alias-duplicate reordering | 1 |

The Class E hit is a **second instance** of the pattern Phase P first
surfaced — selection reordering between BASE and HEAD when an op has
aliased duplicates of the same field. No new class.

Surface activation:
- 73 / 84 schemas have `@key(fields: "sk0")`
- 21 / 84 diffs explicitly reference `sk0` somewhere
- The multi-key surface IS being exercised, but doesn't add to the
  five-class set: the secondary key gives the planner alternative
  join paths between subgraphs, but in this version pair both planners
  consistently pick the same path.

The defer combo (552 divergences) decomposes entirely into the four
known defer-enabled classes (A/B/C/D); no new class with multi-key +
defer either.

## Combined picture (Phases I → Q — 41000+ ops swept)

> Across compound `@key`, **multiple `@key` per entity (different key
> sets)**, multi-field `@requires`, `@override` (incl. progressive),
> `@external`, `@shareable`, interfaces, `@interfaceObject`,
> `@provides`, inter-entity references with multi-hop traversal,
> `@defer`, `__typename` sprinkling, alias-with-skip duplication, and
> named fragment definitions — the planner from 2.5.0 forward is
> byte-identical to 2.13.1 on every generated case.
>
> Total divergence classes between 2.0.0 → 2.13.1:
>   A. PR #7580 (extraneous `... on Query` inline fragment)
>   B. FED-505 / missing Condition node
>   C. Defer wire format: `Defer.primary.sub_selection` field added
>   D. Defer Field representation: string-with-directives → `{response_key}`
>   E. Selection reordering for aliased duplicates
>
> All five fixed in (2.1.3, 2.5.0]. **COVERAGE_GAPS.md is fully closed
> for the 2.5.x → 2.13.x version pair.**

## Curated artifacts

- `class_E_with_secondary_key.txt` — second Class E instance, on a
  schema with `@key(fields: "sk0")` declared. Shows the alias-reorder
  pattern is repeatable, not a one-off.
- `secondary_key_in_diff_pr7580.txt` — divergence with `sk0` visible
  in the subgraph operation document, otherwise the standard PR #7580
  pattern.
- `cross_version_2.0.0_multikey_defer/secondary_key_with_defer.txt` —
  multi-key interacting with defer-C/D classes.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_multikey

cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 --enable-defer \
    --regressions-dir tests/regressions/cross_version_2.0.0_multikey_defer
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
