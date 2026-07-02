### Short-circuit unsatisfied condition resolutions via monotonicity ([PR #9744](https://github.com/apollographql/router/pull/9744))

Adds an unsatisfied-monotonicity optimization to the condition resolver
cache. If a condition was previously resolved as `Unsatisfied` with a
given set of excluded destinations and excluded conditions, it will
remain `Unsatisfied` with any superset of those exclusions — adding
more exclusions can only remove possible paths, never create new ones.

When the cache finds an `Unsatisfied` entry whose exclusion sets are
subsets of the current lookup's exclusion sets, it returns the cached
`Unsatisfied` result immediately without re-evaluating the condition.

Example: A type with three keys across four subgraphs.

```graphql
# Subgraph "A"
type Product @key(fields: "id") @key(fields: "sku") @key(fields: "upc") {
  id: ID!
  sku: String!
  upc: String!
}

# Subgraph "B"
type Product @key(fields: "id") { id: ID!, name: String }

# Subgraph "C"
type Product @key(fields: "sku") { sku: String!, price: Int }

# Subgraph "D"
type Product @key(fields: "upc") { upc: String!, weight: Float }
```

When evaluating whether to reach subgraph D from A, the planner tries
each key in order. Suppose the `id` key edge `A → B` is evaluated with
`excluded_destinations: {B}` and found to be `Unsatisfied` (B can't
reach D). Later, when evaluating a different path with
`excluded_destinations: {B, C}`, the cache sees the previous
`Unsatisfied` result with the smaller exclusion set `{B}`. Since
`{B, C}` is a superset of `{B}`, the condition is still unsatisfiable
— no need to re-evaluate.

A quick `len()` check on both exclusion sets avoids the element-wise
superset comparison in the common case where the cached set is larger
than or equal in size to the lookup set.

By [@tninesling](https://github.com/tninesling)
