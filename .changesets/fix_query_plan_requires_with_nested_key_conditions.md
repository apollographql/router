### Fix query planner error on `@requires` when the key's conditions are fetched below the entity

Planning a query could fail with the internal error `Union types don't have field "<field>", only "__typename"` when an entity's `@requires` had to be resolved through a nested `@key` whose own fields came from another subgraph. Concretely, this happens when the entity's key is nested (for example `@key(fields: "subEntity { id2 }")`) and `id2` is only resolvable elsewhere, so the key-resolution *conditions* get fetched at a path *deeper* in the response (`unionField.subEntity`) than the entity that needs them (`unionField`).

In that situation the condition fetch sits below the key fetch, so there is no downward path from the former to the latter. Three places mishandled that:

- `compute_nodes_for_key_resolution()` subtracted the two paths in the wrong direction, producing a path describing the *reverse* relation. That invalid path was then resolved against the entity fetch's `_Entity` union root, which raised the error above.
- `handle_conditions_tree()` skipped its entire "merge into the grand parent" block when there was no path into the parent, silently dropping the condition fetch nodes it had just created instead of reporting them as created.
- `create_post_requires_node()` assumed a path into the parent existed whenever there was a single parent, and aborted with `Missing path_in_parent for @require` otherwise.

The first defect masked the other two: they would have ordered the `@requires` fetch *before* the fetch producing the required field, so the entity resolver would have been called without its required data. All three are fixed, and such queries now plan correctly, fetching the required field before the field that requires it.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/XXXX
