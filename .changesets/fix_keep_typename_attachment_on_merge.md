### Keep a requested `__typename` when merging equivalent field selections ([PR #10374](https://github.com/apollographql/router/pull/10374))

To plan faster, the query planner temporarily removes a requested `__typename` and records it on a sibling field, then adds it back when building subgraph fetches. When a field carrying that record was merged into an equivalent field without one, the record was dropped and the `__typename` was never added back. This happened, for example, in the plan branch used when a conditional `@defer(if: $var)` is disabled: a `__typename` requested only inside the deferred fragment was missing from the subgraph fetch. The record is now kept when the fields are merged.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10374
