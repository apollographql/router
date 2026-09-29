### Share equal path trees across condition cache entries ([PR #9975](https://github.com/apollographql/router/pull/9975))

The condition resolver cache stores one entry per `(context,
excluded_destinations, excluded_conditions)` combination per edge.
Exclusion sets change on each key-edge attempt during path exploration,
so an edge commonly accumulates many entries whose resolutions are
structurally identical path trees, each independently allocated by a
separate nested traversal.

Cache inserts now compare the new resolution against the edge's
existing entries and, when an equal resolution is already cached, store
a shared reference to the existing path tree instead of retaining
another copy. This reduces the query planner's peak heap usage during
planning. Cache lookup behavior and generated query plans are
unchanged.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/9975
