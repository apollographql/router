### Fix subgraph errors being silently dropped on Flatten fetches crossing a type-conditioned field

A subgraph-level error with no `_entities`-indexed path (for example, the error produced by a `traffic_shaping` timeout) could be silently dropped instead of surfacing to the client, when the erroring fetch's `Flatten` path crossed an abstract-typed (interface or union) field reached through a type condition on a single-valued field rather than on an array wildcard (e.g. `...edges.@.node|[SomeConcreteType].collection`).

The router expands such an error across every entity in the batch by matching the error's declared path (which retains its type condition) against each entity's real, already-materialized path (which never carries one). That match only special-cased array `Index`/`Flatten` pairs and otherwise required exact structural equality, so a `Key` path element with a type condition never matched the same `Key` without one — the error matched none of the batch's entities and was dropped entirely, with no trace in the response's `errors` or anywhere else, while the corresponding data was simply missing.

`Path::equal_if_flattened` now also treats two `Key` path elements as equal whenever their names match, regardless of any type condition attached to either side, so these errors correctly surface once per affected entity instead of vanishing silently.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/PLACEHOLDER
