# Compound `@key` sweep against 2.0.0 — negative result

After Phase J (entities optionally get a compound `@key(fields: "id k0
[k1]")` with extra non-null scalar key components, uniform across all
hosts), pointed the harness at `apollo-federation = "=2.0.0"` for 1000
ops over 200 generated supergraphs:

    Stats { schemas_attempted: 200, schemas_composed: 200,
            schemas_compose_failed: 0, ops_attempted: 1000,
            ops_skipped: 0, planned_identical: 913,
            planned_divergent: 87, planner_errored: 0 }

Categorization of the 87 divergences:

| Category | Count |
|---|---|
| PR #7580 only (`... on Query` extraneous root condition) | 39 |
| PR #7580 + Condition node difference (FED-505) | 48 |
| **Anything new** | **0** |

## Compound-key surface activation

- 72 / 87 divergent files have schemas containing compound `@key`s.
- 38 / 87 diffs explicitly mention compound key field names (`k0`/`k1`).
- The entity-boundary `requires` selection in `Flatten` plan nodes
  consistently includes the compound key, e.g.
  `"requires": ["... on T1 { __typename id k0 }"]`, **identical between
  both planner versions**.

So compound keys are being exercised, both at the schema level and at
the operation/plan level, and the planner's compound-key handling for
entity fetches is stable from 2.0.0 → 2.13.1.

## Conclusion

Compound `@key` does not surface a new bug class on the 2.0.0 → 2.13.1
version pair. Combined with the multi-field `@requires` negative result
from Phase I, the picture is now:

> Across the schema/operation surface this generator covers, the only
> divergence classes between 2.0.0 and 2.13.1 are PR #7580 (extraneous
> `... on Query` inline fragment in subgraph operation documents) and
> FED-505 (missing `Condition` plan node when `@skip`/`@include` could
> elide a fetch). Both have already been raised upstream.

The compound-key surface is kept in the harness — useful for future
runs against other version pairs and for compounding with future
generator additions (e.g. `@interfaceObject`, `@provides`).

## Curated artifact

- `compound_key_in_requires_pr7580.txt` — divergent op against a schema
  with a compound key. The `Flatten` node's `requires` selection
  (`... on T1 { __typename id k0 }`) is identical in both versions; the
  divergence is the known PR #7580 `... on Query` pattern.

## Reproducing

```toml
# apollo-federation-fuzz/Cargo.toml
apollo-federation-base = { package = "apollo-federation", version = "=2.0.0" }
```

```sh
cargo run -p apollo-federation-fuzz --bin fuzz --release -- \
    --iterations 1000 --seed 17 --ops-per-schema 5 \
    --regressions-dir tests/regressions/cross_version_2.0.0_compound_keys
```

The default `Cargo.toml` is back at `=2.13.0` so `cargo test` is clean.
