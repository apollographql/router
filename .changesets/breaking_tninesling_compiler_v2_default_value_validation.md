### Stricter default value validation with `apollo-compiler` v2 ([PR #10119](https://github.com/apollographql/router/pull/10119))

The router now uses `apollo-compiler` v2, which validates default values against the GraphQL September 2025 specification. Supergraphs with invalid default values (e.g. `{}` for an input type with required fields) are now rejected at startup.

A new configuration option `supergraph.validate_default_values` (default `true`) provides an escape hatch. Set it to `false` to accept supergraphs that were composed before this validation existed:

```yaml
supergraph:
  validate_default_values: false
```

By [@tninesling](https://github.com/tninesling) in <https://github.com/apollographql/router/pull/10119>
