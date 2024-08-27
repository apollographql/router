### Support demand control directives ([PR #5777](https://github.com/apollographql/router/pull/5777))

The router supports two new demand control directives, `@cost` and `@listSize`, that you can use to provide more accurate estimates of GraphQL operation costs to the router's demand control plugin.

Use the `@cost` directive to customize the weights of operation cost calculations, particularly for expensive resolvers.

```graphql
type Product {
  id: ID!
  name: String
  expensiveField: Int @cost(weight: 20)
}
```

Use the `@listSize` directive to provide a more accurate estimate for the size of a specific list field, particularly for those that differ greatly from the global list size estimate.

```graphql
type Magazine {
  # This is assumed to always return 5 items
  headlines: [Article] @listSize(assumedSize: 5)

  # This is estimated to return as many items as are requested by the parameter named "first"
  getPage(first: Int!, after: ID!): [Article]
    @listSize(slicingArguments: ["first"])
}
```

To learn more, go to [Demand Control](https://www.apollographql.com/docs/router/executing-operations/demand-control/) docs.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/5777
