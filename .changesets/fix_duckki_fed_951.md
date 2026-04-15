### Fix demand control `_entities` cardinality estimation ([PR #9192](https://github.com/apollographql/router/pull/9192))

The demand control plugin's static cost estimator now derives the `_entities` instance count from the `FlattenNode` path in the query plan, rather than falling back to the global `list_size` default. For nested-list federated queries (e.g. `companies { employees { salary } }` where both fields are lists), this produces a much more accurate upper-bound estimate by multiplying list sizes along the path. Previously, the estimate used the flat `list_size` default regardless of nesting depth, which could both overcount (e.g. `list_size=100` when `_entities` has one item for a single-object parent) and undercount (e.g. `list_size=100` when nested lists produce 10,000 entities).

The `@listSize(assumedSize: N)` directive on supergraph fields is respected when computing path cardinality.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9192