### Entity cache: support several key directives on the same type  ([PR #7207](https://github.com/apollographql/router/pull/7207))

This PR fixes a bug in entity caching introduced by the fix in https://github.com/apollographql/router/pull/6888 for cases where several `@key` directives with different fields were declared on a type as documented [here](https://www.apollographql.com/docs/graphos/schema-design/federated-schemas/reference/directives#managing-types). 

For example if you have this kind of entity in your schema:

```graphql
type Product @key(fields: "upc") @key(fields: "sku") {
  upc: ID!
  sku: ID!
  name: String
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7207