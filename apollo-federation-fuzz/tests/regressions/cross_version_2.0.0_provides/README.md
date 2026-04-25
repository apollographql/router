# `@provides` sweep — negative result

After Phase L (added `@provides(fields: "...")` on Query root fields,
selecting one or two non-key, exclusively-hosted scalar fields owned by
some other subgraph; the providing subgraph marks them `@external` in
its entity declaration, and the owner's declaration adds `@shareable` to
satisfy the federation rule that `@provides` makes the providing
subgraph an additional resolver of the field):

## 2.5.0 → 2.13.1 (4 seeds × 1000 ops = 4000 ops)

    seed 17 :   1000 identical / 0 divergent
    seed 42 :   1000 identical / 0 divergent
    seed 1234:  1000 identical / 0 divergent
    seed 99 :   1000 identical / 0 divergent

**Zero divergences.** `@provides` assembly is byte-stable across the
2.5.x → 2.13.x window for this generator's covered surface.

## 2.0.0 → 2.13.1 (1000 ops, seed 17)

    Stats { ... planned_identical: 913, planned_divergent: 87 ... }

| Category | Count |
|---|---|
| PR #7580 only | 40 |
| PR #7580 + Condition node difference (FED-505) | 47 |
| **Anything new** | **0** |

`@provides` surface IS exercised: 36 / 87 divergent schemas contain
`@provides`. None of the divergences are caused by the `@provides` code
path itself — the planner emits the same `@provides`-aware plan in
both versions; the divergences are all in the unrelated PR #7580 /
FED-505 patterns the harness has already fully characterised.

## Conclusion

Combined picture from Phases I, J, L (multi-field `@requires`, compound
`@key`, `@provides`):

> Across the schema/operation surface this generator covers — single
> and compound keys, `@requires` (single and multi-field), `@override`
> (with progressive labels), `@external`, `@shareable`, interfaces, and
> `@provides` — the planner from 2.5.0 forward is byte-identical to
> 2.13.1 on every generated case (12000+ ops swept).
>
> The only divergence classes between 2.0.0 → 2.13.1 are PR #7580 and
> FED-505. Both fixes landed in (2.1.3, 2.5.0].

The surface is kept in the harness. Future opportunities to surface
*new* bugs likely require:

- A genuinely new directive class (`@interfaceObject`, `@requiresScopes`,
  context features).
- A richer operation generator (deeper fragments, named definitions,
  more shape variety than apollo-smith currently produces).
- Semantic comparison: issue ops against an executor backed by both
  planners, compare *responses* — catches "both planners agree on a
  wrong answer" which is currently invisible.

## Curated artifact

- `provides_present_pr7580_fed505.txt` — one of the 87 divergences with
  `@provides` in the schema. Both versions emit the same `@provides`-
  aware plan; the diff is the standard PR #7580 + Condition node
  pattern.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_provides
```

The default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
