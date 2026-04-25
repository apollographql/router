# `@interfaceObject` sweep — negative result

After Phase N (added `@interfaceObject` declarations: when an
`InterfacePlan` exists and the augmentation fires, a separate subgraph
that does *not* host any of the interface's implementers emits
`type I0 @key(fields: "id") @interfaceObject { id: ID! [+fields] }`,
the interface's host gains `@key(fields: "id")`, and implementers with
compound primary keys get a fallback `@key(fields: "id")` for federation
matching-key compliance):

## Sweeps

| BASE -> 2.13.1                                                    | Iter / op | Divergences |
|--------------------------------------------------------------------|-----------|-------------|
| `=2.5.0`, no defer (seed 17)                                       | 1000      | **0**       |
| `=2.5.0`, --enable-defer (seeds 17, 42, 1234, 99 — 4000 ops)       | 4 × 1000  | **0**       |
| `=2.0.0`, no defer (seed 17)                                       | 1000      | 85          |
| `=2.0.0`, --enable-defer (seed 17)                                 | 1000      | 545         |

Composition rate held at 100% (after the federation rule "@interfaceObject
host must not host any implementers" was honoured by the candidate
filter).

## Findings

### 2.5.0 → 2.13.1: still byte-identical

Across **5000 ops** with `@interfaceObject` enabled (1000 no-defer +
4000 with defer), zero plan divergences. PR #8109 (`__typename` on
`@interfaceObject` types) territory is presumably intact since 2.5.0,
or the patterns this generator produces don't trigger it.

### 2.0.0 → 2.13.1: same divergence classes as before

| no-defer (85) | with-defer (545) |
|---|---|
| Contains PR #7580: 85 | Contains PR #7580: ~200 |
| Contains Condition diff: 49 | Contains Condition diff: ~390 |
| Contains `@interfaceObject` in schema: 20 | Contains `@interfaceObject` in schema: ~110 |
| Contains `interfaceObject`/`isInterfaceObject` in diff: 0 | (small, unchanged) |

No divergence diff explicitly references `@interfaceObject` plan
machinery. The `@interfaceObject` surface is being exercised by ~24%
of divergent schemas, but the 2.0.0 vs 2.13.1 plan differences in
those cases are still the previously-characterised PR #7580 / FED-505
/ defer-C / defer-D patterns — `@interfaceObject` doesn't add a new
divergence class on this version pair.

## Combined picture (Phases I, J, K, L, M, N)

> Across the full schema/operation surface this generator now covers —
> single + compound `@key`, single + multi-field `@requires`, `@override`
> (incl. progressive labels), `@external`, `@shareable`, interfaces,
> `@interfaceObject`, `@provides`, and (separately) `@defer` decoration
> on operations — the planner from 2.5.0 forward is byte-identical to
> 2.13.1 on every generated case (≥ 17000 ops total).
>
> Total divergence classes between 2.0.0 → 2.13.1:
>   A. PR #7580 (extraneous `... on Query` inline fragment)
>   B. FED-505 / missing Condition node (over-fetching on @skip/@include)
>   C. Defer wire format: `Defer.primary.sub_selection` field added
>   D. Defer Field representation: string-with-directives → `{response_key}`
>
> All four fixed in (2.1.3, 2.5.0].

The `@interfaceObject` surface is kept in the harness — useful as
coverage for future runs against newer planner versions or for
combining with future generator additions.

## Curated artifacts

- `interfaceobject_present_pr7580.txt` — 2.0.0 divergence with
  `@interfaceObject` in the schema (20/85 of the no-defer 2.0.0
  divergences). The diff is the standard PR #7580 pattern; the
  interface-object plan machinery agrees between versions.

(A second artifact `interfaceobject_with_defer_subselection.txt` lives
in `cross_version_2.0.0_interfaceobject_defer/` showing how
`@interfaceObject` co-occurs with the defer-C `sub_selection`
divergence. Same defer-class pattern — no new bug class introduced by
the interaction.)

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_interfaceobject

# With defer:
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 --enable-defer \
    --regressions-dir tests/regressions/cross_version_2.0.0_interfaceobject_defer
```

Default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
