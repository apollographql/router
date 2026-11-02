### Add incremental planner entry point and router configuration ([PR #10107](https://github.com/apollographql/router/pull/10107))

Adds the incremental (BULB) query planner as a planner variant behind configuration. The planner constructs plans field-by-field with bounded backtracking instead of exhaustively enumerating plan candidates.

The new `supergraph.query_planning.incremental_planner` configuration section exposes:

- `enabled` (default: `false`) - enable the incremental planner.
- `beam_width` (default: `16`) - how many states advance together per search depth. Wider beams capture more diversity, reducing expensive backtracking.
- `fuel` (default: `5000`) - cap on optimization effort beyond the first draft of the plan, measured in pending-selection visits.
- `timeout` (optional) - wall-clock time limit for the search, e.g. `5s`. When set, the search returns the best complete plan found so far once the limit is reached.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10107
