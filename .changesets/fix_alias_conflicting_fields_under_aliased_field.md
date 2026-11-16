### Alias conflicting subgraph fields nested under an aliased field ([PR #10523](https://github.com/apollographql/router/pull/10523))

When a subgraph fetch selects two fields with the same response name but incompatible types, the query planner aliases one of them and renames it back after the fetch. If such a conflict was nested inside a field that was itself aliased (for example `o: P!` on one type and `o: P` on another, with `x: Int!` and `x: Int` selected inside both, as `@requires` inputs can do), the planner recorded the inner rename but never applied the inner alias. Query planning then failed with "Query planning produced an invalid subgraph operation ... must not select different types using the same name".

The inner alias is now applied, and its rename runs after the outer field has been renamed back, so these operations plan and execute normally.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10523
