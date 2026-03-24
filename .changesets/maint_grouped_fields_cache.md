### Eliminate per-object allocation in response formatting by pre-computing grouped fields ([Issue #XXXXXX](https://github.com/apollographql/router/issues/XXXXXX))

Response formatting previously traversed the query's selection set and fragment tree from scratch for every object in a subgraph response, allocating and populating a field-grouping map on each call.  For responses with many objects — large lists, deeply nested unions — this produced measurable overhead under load.

The router now pre-computes the grouped field layout for each `(selection_set, concrete_runtime_type)` pair the first time a query is formatted, and reuses those pre-computed groups for every subsequent object in the same response.  The computation happens lazily on the first `format_response` call and is stored in the parsed `Query` struct, which is already cached per-operation.  The observable behavior is unchanged.

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/XXXXXX
