# Multi-field `@requires` sweep against 2.0.0 — negative result

After Phase I (extended `@requires` to optionally emit
`@requires(fields: "f g")` with two providers in different subgraphs
becoming `@external` in the requirer's subgraph), pointed the harness at
`apollo-federation = "=2.0.0"` for 1000 ops:

    Stats { schemas_attempted: 200, schemas_composed: 200,
            schemas_compose_failed: 0, ops_attempted: 1000,
            ops_skipped: 0, planned_identical: 890,
            planned_divergent: 110, planner_errored: 0 }

110 divergences (vs 92 with interfaces alone, 92 without multi-field
requires). Categorization:

| Category | Count |
|---|---|
| PR #7580 only | 41 |
| PR #7580 + Condition node difference (FED-505) | 69 |
| **Anything new** | **0** |

Spot-checked schemas containing multi-field `@requires` directly with
`dump_one` — plans **agree** across both planner versions. Multi-field
`@requires` assembly is well-tested and stable from 2.0.0 → 2.13.1.

The slight bump in divergence rate (92 → 110) is consistent with multi-
field requires causing slightly broader op-generation surface (more
external/required fields available for apollo-smith to select), giving
existing PR #7580 / FED-505 patterns more opportunities to fire.

## Conclusion

Multi-field `@requires` alone does not surface a new bug class on the
2.0.0 → 2.13.1 version pair. The surface is still valuable to keep in
the harness:

- Future planner changes may regress assembly logic — having coverage
  catches that.
- Combined with future generator additions (chained @requires through a
  third subgraph, multi-host @external, compound @keys), this could
  compound into new findings.

`requires_present_pr7580.txt` is one captured case where the schema has
`@requires` and the divergence is the known PR #7580 pattern.
