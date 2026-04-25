# Point-release sweep: 2.13.0 → 2.13.1

Sanity check that no plan-observable regression was introduced between
the immediate predecessor and the in-tree HEAD.

    Stats { schemas_attempted: 200, schemas_composed: 200,
            schemas_compose_failed: 0, ops_attempted: 1000,
            ops_skipped: 0, planned_identical: 1000,
            planned_divergent: 0, planner_errored: 0 }

**Zero divergences.** Both versions emit byte-identical plan documents
across 1000 generated ops over 200 supergraphs — what you'd expect
from a clean point release.

Kept as a regression baseline: if a future patch release diverges
against 2.13.0, this sweep will catch it.
