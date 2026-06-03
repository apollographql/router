### Fix demand control `_entities` cardinality estimation ([PR #9192](https://github.com/apollographql/router/pull/9192))

When estimating the cost of a federated operation, the demand control plugin sized every entity fetch (`_entities`) with the flat global `list_size` default, ignoring how many parent objects the fetch actually extends. For nested-list operations (e.g. `topProducts { reviews { author { name } } }`) this could both overcount (an `_entities` fetch often resolves a single object per parent) and undercount (nested lists can produce far more entities than `list_size`), so the estimate could wrongly reject inexpensive operations or admit expensive ones.

The estimator now counts entity representations from the parent list sizes along the response path, using the same list sizing as the rest of cost estimation: the `@listSize` directive (`assumedSize`, `slicingArguments`, and `sizedFields`) and per-subgraph `list_size` configuration. Because an entity fetch is sized by the list it is fetched through, its count reflects that subgraph's configured `list_size` rather than the global default. As a result, estimated costs for operations with entity fetches may change.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9192
