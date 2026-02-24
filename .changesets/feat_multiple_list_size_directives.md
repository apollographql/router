### Support multiple `@listSize` directives on the same field ([PR #8872](https://github.com/apollographql/router/pull/8872))

> [!WARNING]
>
> Multiple `@listSize` directives on a field only take effect after Federation supports repeatable `@listSize` in the supergraph schema. Until then, composition continues to expose at most one directive per field. This change makes the router ready for that Federation release.

The router now supports multiple `@listSize` directives on a single field, enabling more flexible cost estimation when directives from different subgraphs are combined during federation composition.

- The router processes all `@listSize` directives on a field (stored as `Vec<ListSizeDirective>` instead of `Option<ListSizeDirective>`).
- When multiple directives specify `assumedSize` values, the router uses the maximum value for cost calculation.
- Existing schemas with single directives continue to work exactly as before.

This change prepares the router for federation's upcoming support for repeatable `@listSize` directives, and maintains full compatibility with current non-repeatable directive schemas.

By [@cmorris](https://github.com/cmorris) in https://github.com/apollographql/router/pull/8872
