### Remove the legacy query planner ([PR #6418](https://github.com/apollographql/router/pull/6418))

The legacy query planner has been removed in this release. In the previous release, router v1.58, it was no longer used by default but was still available through the `experimental_query_planner_mode` configuration key. That key is now removed.

Also removed are configuration keys which were only relevant to the legacy planner:

* `supergraph.query_planning.experimental_parallelism`: the new planner can always use available parallelism.
* `supergraph.experimental_reuse_query_fragments`: this experimental algorithm that attempted to
reuse fragments from the original operation while forming subgraph requests is no longer present. Instead, by default new fragment definitions are generated based on the shape of the subgraph operation.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6418
