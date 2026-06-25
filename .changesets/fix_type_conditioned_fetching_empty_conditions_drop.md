### Fix entity representations silently dropped when `experimental_type_conditioned_fetching` is enabled

When `experimental_type_conditioned_fetching: true`, certain entity types could have their representations silently dropped during Flatten-path execution, causing those types to disappear from responses while other types in the same union or interface resolved correctly.

The root cause was a change to `FetchDataPathElement` that made type conditions optional (`Option<Conditions>` instead of bare `Conditions`). The corresponding conversion in `convert.rs` was updated to use `.as_ref().map(…)`, which preserved `Some([])` (Some with an empty conditions list) unchanged. In the router's `iterate_path`, `Some([])` is interpreted as "filter everything" — neither the type-condition-filtered branch nor the unfiltered branch is entered — so all entities at that path position are silently dropped.

The fix restores the pre-change semantics: an empty conditions list means "no type filtering" and is converted to `None` at the federation-to-router boundary. The query planner is also updated to emit `None` directly when the computed type conditions set is empty, preventing the bad value from being generated in the first place.

By [@zachfettersmoore](https://github.com/zachfettersmoore) in https://github.com/apollographql/router/pull/PULL_NUMBER
