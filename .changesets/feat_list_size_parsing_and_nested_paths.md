### Improve `@listSize` directive parsing and nested path support

Demand control cost calculation now supports:

- **Array parsing for `@listSize`:** List-size directives can use array-style parsing for sizing (e.g. list arguments).
- **Nested input paths:** Nested input paths are supported when resolving list size from query arguments.
- **Nested `sizedFields`:** The `sizedFields` argument on `@listSize` supports nested field paths for more accurate cost estimation.

These changes are backward compatible with existing schemas and directives.

By [@cmorris](https://github.com/cmorris) in https://github.com/apollographql/router/pull/8893
