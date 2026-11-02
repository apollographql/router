### Keep `@deprecated` on fields an interface does not declare ([PR #10460](https://github.com/apollographql/router/pull/10460))

The federation 3 compatibility upgrade strips `@deprecated` from an implementing field when the matching interface field is not deprecated, because the GraphQL September 2025 spec disallows it. It applied that check to every field on the implementing type, so fields the interface does not declare also lost their `@deprecated`, and composition emitted a spurious `DEPRECATED_IMPLEMENTING_FIELD_WITHOUT_INTERFACE` hint.

Only fields the interface declares without `@deprecated` are now stripped.

```graphql
interface Node { id: ID! }
type User implements Node {
  id: ID!
  # previously lost its @deprecated during composition
  name: String @deprecated(reason: "use displayName")
}
```

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10460
