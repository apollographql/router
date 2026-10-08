### Preserve `@skip`/`@include` on entity fetches that follow a `@requires`

When an operation selected two fields under opposite conditions on the same variable — for example
`...A @include(if: $v)` and `...B @skip(if: $v)` — and both fields carried a `@requires` satisfied by
another subgraph, the query planner dropped both conditions from the entity fetch that resolved
them. The two conditional branches collapsed into a single unconditional fetch, so a subgraph could
be asked to resolve a field the client had explicitly excluded.

Where the two `@requires` field sets differed, the merged fetch also demanded input fields that were
only ever fetched on the other branch, which could surface as errors or nulls from the subgraph.

The conditions in force at a `@requires` edge are recorded in the fetch node's path rather than in
the `OpGraphPathContext` used to rebuild that path, because the context only accumulates conditionals
across subgraph-jump edges. They are now folded in, so the branches stay distinct and each carries
its own condition and its own `@requires` inputs.

Note that two branches which previously merged into one entity fetch will now be planned as two.
That is required for correctness, but it is one additional subgraph fetch for operations that were
relying on the merged shape.

By [@andy.garcia](https://github.com/andygarcia) in https://github.com/apollographql/router/pull/10063
