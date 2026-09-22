### Stricter `@deprecated` directive validation with `apollo-compiler` v2 ([PR #10119](https://github.com/apollographql/router/pull/10119))

The router now uses `apollo-compiler` v2, which enforces three new `@deprecated` validation rules from the GraphQL September 2025 specification:

- `@deprecated(reason: null)` is no longer valid. The `reason` argument is now typed `String!`, so it must be a string or omitted entirely.
- `@deprecated` must not appear on required arguments or input object fields (non-null with no default value).
- If an object or interface field is marked `@deprecated`, the interface field it implements must also be deprecated.

Supergraphs that were composed by an older composition version and contain violations of these rules will be rejected at router startup. To resolve this, recompose the supergraph with the latest LTS version of composition, or update your affected subgraphs.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10119
