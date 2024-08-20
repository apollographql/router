### Account for demand control directives when scoring operations ([PR #5777](https://github.com/apollographql/router/pull/5777))

When scoring operations in the demand control plugin, utilize applications of `@cost` and `@listSize` from the supergraph schema to make better cost estimates.

For expensive resolvers, the `@cost` directive can override the default weights in the cost calculation.

```graphql
type Product {
  id: ID!
  name: String
  expensiveField: Int @cost(weight: 20)
}
```

Additionally, if a list field's length differs significantly from the globally-configured list size, the `@listSize` directive can provide a tighter size estimate.

```graphql
type Magazine {
  # This is assumed to always return 5 items
  headlines: [Article] @listSize(assumedSize: 5)

  # This is estimated to return as many items as are requested by the parameter named "first"
  getPage(first: Int!, after: ID!): [Article]
    @listSize(slicingArguments: ["first"])
}
```

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5777
