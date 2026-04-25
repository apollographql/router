# `@override` surface sweep against 2.0.0

After Phase G (added `@override` and progressive `@override(label:)` to the
generator), pointed the harness at `apollo-federation = "=2.0.0"` for 1000
ops over 200 generated supergraphs:

    Stats { schemas_attempted: 200, schemas_composed: 200,
            schemas_compose_failed: 0, ops_attempted: 1000,
            ops_skipped: 0, planned_identical: 882,
            planned_divergent: 118, planner_errored: 0 }

**118 divergences (11.8%)**, up from 95 (9.5%) before `@override`. All 118
remain the PR #7580 pattern (`... on Query` → `...`) — the `@override`
surface itself produced **no novel divergence categories**.

The increase is consistent with operations now selecting a wider set of
fields (the `@override`-decorated fields contribute to the field pool),
which gives the renamed-root surface more chances to fire.

## Conclusion

`@override` (including progressive `label:`) appears stable in the planner
across 2.0.0 → 2.13.1 for graphs without interfaces. PR #7929 (progressive
override on interface implementations) cannot trigger here because we don't
generate interfaces yet. Adding interfaces would directly probe that gap.

`override_active_repro.txt` is one captured case where the schemas contain
`@override(from: "...")` and the plan still diverges on the PR #7580
pattern. Confirms the harness exercises `@override` end-to-end without
incident.
