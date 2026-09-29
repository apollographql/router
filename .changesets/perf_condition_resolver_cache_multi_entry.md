### Cache condition resolutions across all context/exclusion combinations ([PR #9741](https://github.com/apollographql/router/pull/9741))

Extends the condition resolver cache introduced in #9740 to cache
resolutions for all combinations of `@include`/`@skip` context and
excluded conditions, not just the first combination seen per edge.

Previously, the cache stored a single `(resolution, excluded_destinations)`
pair per edge, and bailed out entirely when the `OpGraphPathContext`
(active `@include`/`@skip` directives) or `ExcludedConditions` were
non-empty. For types with multiple `@key` directives, this meant only
the first key's resolution was cached — all subsequent keys were
re-evaluated from scratch every time they were encountered.

Now the cache stores a `Vec<CachedConditionEntry>` per edge, where each
entry records the full `(context, excluded_destinations,
excluded_conditions)` triple. On lookup, entries are scanned for an
exact match. On miss (no matching entry found), the new resolution is
inserted — allowing the cache to accumulate results across all
combinations encountered during planning.

Example: A `Product` type with two keys across three subgraphs.

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

When the planner evaluates how to reach `pricing` and `inventory` from
`products`, it tries each key edge in order. The first key (`id`) is
evaluated with `excluded_destinations: {pricing}`, and the second key
(`sku`) is evaluated with `excluded_destinations: {pricing, inventory}`.
These are different exclusion sets, so each produces a distinct cache
entry for the same edge.

With the old single-entry cache, only the first key's resolution was
stored. The second key hit a different `excluded_destinations` value,
returned `NotApplicable`, and was re-evaluated from scratch — every
time, across every root field that touches `Product`.

With the multi-entry cache, both resolutions are stored and looked up
on subsequent encounters. Every additional root field returning
`Product` reuses both cached key resolutions instead of re-evaluating
them.

The same applies to `@requires` conditions evaluated under different
`@include`/`@skip` contexts — each distinct `(context,
excluded_destinations, excluded_conditions)` combination is cached and
reused independently.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/9741
