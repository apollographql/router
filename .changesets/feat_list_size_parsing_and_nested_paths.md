### Improve `@listSize` directive parsing and nested path support ([PR #8893](https://github.com/apollographql/router/pull/8893))

Demand control cost calculation now supports:

- Array-style parsing for `@listSize` sizing (for example, list arguments)
- Nested input paths when resolving list size from query arguments
- Nested field paths in the `sizedFields` argument on `@listSize` for more accurate cost estimation

These changes are backward compatible with existing schemas and directives.

By [@cmorris](https://github.com/cmorris) in https://github.com/apollographql/router/pull/8893
