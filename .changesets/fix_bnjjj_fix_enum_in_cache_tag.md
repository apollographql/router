### Support enum types in `@cacheTag` directive format ([PR #8496](https://github.com/apollographql/router/pull/8496))

Composition validation no longer raises an error when using enum types in the `@cacheTag` directive's `format` argument. Previously, only scalar types were accepted.

Example:

```graphql
type Query {
  testByCountry(id: ID!, country: Country!): Test
    @cacheTag(format: "test-{$args.id}-{$args.country}")
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8496