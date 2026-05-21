### Fix `@cost` directive weights to support serialized numbers ([PR #9484](https://github.com/apollographql/router/pull/9484))

The `@cost` directive previously defined `weight` as `Int!`, which prevented schemas from using fractional weights allowed by the GraphQL cost directive proposal.

The cost specification now defines `weight` as `String!`, and the router parses serialized numeric values as finite floating-point weights. Existing integer literals are still accepted during schema expansion and normalized to strings, preserving compatibility with existing subgraphs while allowing values such as `@cost(weight: "0.5")`.

By [@ashokk1990](https://github.com/ashokk1990) in https://github.com/apollographql/router/pull/9484
