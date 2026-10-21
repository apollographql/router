### Preserve client aliases that look like planner-generated aliases ([PR #10372](https://github.com/apollographql/router/pull/10372))

When two fields in a subgraph fetch share a response name but can't be merged (for example `x: a` and `x: b` in sibling fragments on different union members), the query planner aliases one of them (`x__alias_0`, `x__alias_1`, ...) and renames the key back after the fetch. The planner picked the first such name not used by an *earlier* field, so it could reuse a name the client requested *later* in the same selection set. The two fields then collapsed into one in the subgraph operation, and renaming the generated alias back removed the client's key, so the router returned `null` for it. For example, `{ node { ... on A { x: a } ... on B { x: b x__alias_0: b } } }` returned `"x__alias_0": null` for `B` nodes.

Generated aliases now avoid every response name already requested at the same level, wherever it appears, so the client's field is always fetched and returned.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10372
