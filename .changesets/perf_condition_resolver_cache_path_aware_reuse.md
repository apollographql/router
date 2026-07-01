### Reuse cached satisfied condition resolutions when the path avoids newly-excluded destinations ([PR #9742](https://github.com/apollographql/router/pull/9742))

Adds path-aware reuse to the condition resolver cache. When looking up
a cached `Satisfied` resolution for an edge, the cache now checks
whether the resolution's path tree traverses any of the
newly-excluded destinations. If it doesn't, the cached result is
reused instead of re-resolving the condition from scratch.

Previously, the multi-entry cache (#9741) required an exact match on
`excluded_destinations` to return a cache hit. This meant that even
when a cached resolution's path was still perfectly valid — because it
didn't go through any of the newly-excluded subgraphs — the cache
would miss and trigger a full re-evaluation.

Example: Continuing from the multi-key `Product` example in #9741.

```graphql
# Subgraph "products"
type Product @key(fields: "id") @key(fields: "sku") {
  id: ID!
  sku: String!
}

# Subgraph "pricing"
type Product @key(fields: "id") { id: ID!, price: Int }

# Subgraph "inventory"
type Product @key(fields: "sku") { sku: String!, inStock: Boolean }
```

```graphql
{ product { price inStock } }
```

Step 1 — evaluating the `id` key: The planner evaluates the key edge
`products → pricing` with `excluded_destinations: {pricing}`. The
condition `{ id }` is satisfied via `products` alone. The cache stores:

```
edge:                   products → pricing
excluded_destinations:  {pricing}
resolution:             Satisfied (path through: products)
used_subgraphs:         {products}
```

Step 2 — evaluating the `sku` key: Now the planner evaluates
`products → inventory` with `excluded_destinations: {pricing, inventory}`.
But it also needs to re-check the `id` key edge `products → pricing`
under this new, larger exclusion set. Without path-aware reuse, this is
a cache miss — the exclusion sets don't match exactly — and the planner
re-evaluates the condition from scratch.

With path-aware reuse, the cache sees that the cached `Satisfied`
resolution for the `id` key only used `{products}` as its path. The
newly-excluded destination `{inventory}` doesn't appear in
`{products}`, so the cached path is still available. Cache hit — no
re-evaluation needed.

This is safe because the path tree records which subgraphs were used to
satisfy the condition (via `collect_subgraphs`). If none of those
subgraphs appear in the new exclusions, the path remains available and
the resolution is still valid. Excluded conditions still require an
exact match, because fewer condition exclusions could open up better
paths that weren't explored when the cached result was computed.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/9742
