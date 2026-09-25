### Introspection `includeDeprecated` arguments are now `Boolean!` ([PR #10119](https://github.com/apollographql/router/pull/10119))

The `includeDeprecated` argument on `__Type.fields`, `__Type.enumValues`, and `__Type.inputFields` changed from `Boolean` to `Boolean!` in the introspection schema. This aligns with the GraphQL September 2025 specification. Clients that rely on introspection schema types (e.g. codegen tools) may see updated type signatures.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10119
