### Support multiple `@listSize` directives on the same field

> [!WARNING]
>
> Multiple `@listSize` directives on a field will only take effect once **Federation** supports repeatable `@listSize` in the supergraph schema. Until then, composition will continue to expose at most one directive per field. This change makes the router ready for that Federation release.

The router now supports multiple `@listSize` directives on a single field, allowing more flexible cost estimation when directives from different subgraphs are combined during federation composition.

**Key changes:**

- **Multiple directive handling:** The router now processes all `@listSize` directives on a field (stored as `Vec<ListSizeDirective>` instead of `Option<ListSizeDirective>`)
- **Maximum value selection:** When multiple directives specify `assumedSize` values, the router uses the maximum value for cost calculation
- **Backward compatible:** Existing schemas with single directives continue to work exactly as before

This change prepares the router for federation's upcoming support for repeatable `@listSize` directives, while maintaining full compatibility with current non-repeatable directive schemas.

By [@cmorris](https://github.com/cmorris) in https://github.com/apollographql/router/pull/8872
