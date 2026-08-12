### Share equal path trees across condition cache entries ([PR #9975](https://github.com/apollographql/router/pull/9975))

The condition resolver cache stores one entry per `(context, excluded_destinations, excluded_conditions)` combination per edge. Exclusion sets change on each key-edge attempt during path exploration, so an edge commonly accumulates many entries whose resolutions are structurally identical path trees — each independently allocated by a separate nested traversal, since a cache miss always runs a fresh `QueryPlanningTraversal` that builds its own `OpPathTree`.

This change dedupes at insert time: when a new resolution's path tree is structurally equal to one already cached for the same edge, the existing entry's `Arc<OpPathTree>` is reused instead of retaining another copy. Cache lookup behavior, hit/miss outcomes, and generated query plans are unchanged — this only reduces the planner's peak heap usage during planning.

By [@tninesling](https://github.com/tninesling) in <https://github.com/apollographql/router/pull/9975>
