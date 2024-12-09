### Remove the legacy query planner ([PR #6418](https://github.com/apollographql/router/pull/6418))

The legacy query planner has been removed in this release. In the previous release, Router 1.58, it was already no longer used by default but it was still available through the `experimental_query_planner_mode`  configuration key. That key is now removed.

Also removed is the `supergraph.query_planning.experimental_parallelism` configuration key which was only relevant to the legacy planner. The new planner can always use available parallelism.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6418
