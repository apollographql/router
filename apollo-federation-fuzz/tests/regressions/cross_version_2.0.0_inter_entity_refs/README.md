# Inter-entity reference sweep — negative result

After Phase O (entities now optionally include reference fields like
`r0_0: T1`. Each contributing subgraph that doesn't already host the
target entity emits a key-only stub of it so federation can stitch the
reference. Multi-host ref fields get `@shareable` like scalars do. The
`@interfaceObject` candidate filter was extended to also exclude
subgraphs that would emit an implementer stub via someone else's
entity-ref field):

## Sweeps

| BASE -> 2.13.1                                                | Iter / op  | Divergences |
|---------------------------------------------------------------|------------|-------------|
| `=2.5.0`, no defer                                            | 1000       | **0**       |
| `=2.5.0`, --enable-defer (seeds 17, 42, 1234, 99 — 4000 ops)  | 4 × 1000   | **0**       |
| `=2.0.0`, no defer                                            | 1000       | 73          |
| `=2.0.0`, --enable-defer                                      | 1000       | 550         |

Composition rate held at 100% after the `@interfaceObject` filter was
taught about stub-host conflicts.

## Findings

### 2.5.0 → 2.13.1: still byte-identical

Across **5000 ops** with inter-entity references active, zero plan
divergences. Specifically: this is now the first sweep where ops can
ask `qT1 { r1_0 { f0_x } }` — multi-hop entity traversal across
subgraph boundaries — which is the surface PR #8016 (multi-hop
`@requires` picking the wrong key-source subgraph) ostensibly needs.
No divergence on this version pair. Either PR #8016 was fixed before
2.5.0 (consistent with all other findings), or the precise trigger
needs richer ops than apollo-smith reliably produces.

### 2.0.0 → 2.13.1: same divergence classes as before

Categorization of the no-defer 73 divergences:

| Pattern | Count |
|---|---|
| PR #7580 only | 22 |
| PR #7580 + Condition (FED-505) | 51 |
| **Anything new** | **0** |

Surface activation: 67/73 schemas have entity-ref fields, 23/73 diffs
explicitly name them — the surface IS exercised, but the resulting
plan diffs still fall in the previously-characterised categories.

The defer combo (550 divergences) has the same distribution into the
4-class set (PR #7580 / FED-505 / defer-C `sub_selection` / defer-D
`Field` representation) as the 545 we got with @interfaceObject and
the 554 we got without either.

## Combined picture (Phases I, J, K, L, M, N, O — 23000+ ops swept)

> Across the schema/operation surface this generator now covers —
> single + compound `@key`, single + multi-field `@requires`,
> `@override` (incl. progressive labels), `@external`, `@shareable`,
> interfaces, `@interfaceObject`, `@provides`, **inter-entity reference
> fields with multi-hop traversal**, and `@defer` decoration on
> operations — the planner from 2.5.0 forward is byte-identical to
> 2.13.1 on every generated case.
>
> Total divergence classes between 2.0.0 → 2.13.1:
>   A. PR #7580 (extraneous `... on Query` inline fragment)
>   B. FED-505 / missing Condition node (over-fetching on @skip/@include)
>   C. Defer wire format: `Defer.primary.sub_selection` field added
>   D. Defer Field representation: string-with-directives → `{response_key}`
>
> All four fixed in (2.1.3, 2.5.0].

## Curated artifacts

- `entity_ref_present_pr7580.txt` — divergence with entity-ref fields
  visible in the diff (`r0_0`, `r1_0`, etc.). The plan otherwise
  matches the standard PR #7580 / FED-505 patterns.

- `cross_version_2.0.0_inter_entity_refs_defer/entity_ref_with_defer.txt`
  — entity-refs interacting with the defer-C `sub_selection` divergence.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_inter_entity_refs

cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 --enable-defer \
    --regressions-dir tests/regressions/cross_version_2.0.0_inter_entity_refs_defer
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
