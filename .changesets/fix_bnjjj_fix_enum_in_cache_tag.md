### Do not raise an error when using enum in cacheTag directive format ([PR #8496](https://github.com/apollographql/router/pull/8496))

Fix composition validation when checking `@cacheTag` format used with an enum.

Example:

```graphql
type Query {
    testByCountry(id: ID!, country: Country!): Test @cacheTag(format: "test-{$args.id}-{$args.country}" ) # This was throwing an error because of Country being an enum and not a Scalar type
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8496