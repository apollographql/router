### Share condition resolver cache across recursion depths ([PR #9721](https://github.com/apollographql/router/pull/9721))

Reduces query planning time for operations on schemas with many
`@key`/`@requires` conditions by sharing the condition resolver cache
across recursion depths.

During query planning, the planner evaluates whether key edges (like
`@key` or `@requires`) can be satisfied by resolving their conditions.
When a key has a compound condition (e.g., `@key(fields: "id sku")`) and
the current subgraph doesn't have all the required fields, the planner
creates an inner traversal, a recursive planning step that figures
out how to obtain the missing fields. Previously, each inner traversal
created its own fresh cache. Results discovered during the inner
traversal were discarded when the recursion unwound, so the same
conditions were re-evaluated from scratch every time they were
encountered.

Now the cache is owned at the top level and threaded via
`&mut ConditionResolverCache` through the entire call stack, so results
discovered at any recursion depth are visible to all others.

Example: Two query root fields return the same entity type.

```graphql
# Subgraph "products"
type Query {
  featured: Product
  recommended: Product
}
type Product @key(fields: "id") { id: ID! }

# Subgraph "details"
type Product @key(fields: "id") { id: ID!, price: Int }
```

```graphql
{ featured { price } recommended { price } }
```

Step 1 — planning `featured`: The planner starts at `products`
(the entry subgraph for `Query.featured`). `products` doesn't have
`price`, so the planner evaluates the key edge `products → details` —
checking whether the key condition `{ id }` can be satisfied. It can
(`products` has `id`), so the edge is Satisfied. This result gets cached:

```
edge:                   products → details
excluded_destinations:  {details}
resolution:             Satisfied
```

Step 2 — planning `recommended`: The planner moves to
`Query.recommended`. It faces the exact same entity type, the exact same
key edge `products → details`, and the exact same exclusion set
`{details}`.

Without the shared cache, the planner would create a fresh cache for
this subtree and re-evaluate every condition from scratch — duplicating
all the work from step 1.

With the shared cache, the planner finds an exact-match hit on the
cached entry from step 1. No re-evaluation needed.

The win scales with the number of root fields and entity types — every
additional field that touches the same entity type reuses all the
condition resolutions that were already computed.

By [@tninesling](https://github.com/tninesling) in <https://github.com/apollographql/router/pull/9721>
