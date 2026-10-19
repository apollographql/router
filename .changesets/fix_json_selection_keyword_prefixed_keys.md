### Fix connector mappings silently misreading names that begin with a keyword or a number ([PR #10260](https://github.com/apollographql/router/pull/10260))

`JSONSelection` could stop reading a name partway through and treat the rest as a separate selection:

- **Names beginning with `null`, `true`, or `false`.** `displayName: nullableName` parsed as `displayName: null ableName`, assigning `null` to `displayName` and adding a selection for `ableName`. In other positions, such as `$(nullField ?? "fallback")`, it failed with an opaque `nom::error::ErrorKind::Eof`.
- **Paths rooted at a number.** `alias: 1.foo` parsed as `alias: 1.0 foo`, and `$(1.foo)` failed to parse.

If a connector mapping aliases a field name starting with `null`, `true`, or `false`, or a path rooted at a number, its output changes from `null` to the intended value. Bare keys like `nullableName` or `outer { nullableName }` were never affected.

A selection list also now rejects two items that abut with an identifier character on each side and nothing between them, so input like `alias: 1b: 2` is an error rather than two selections. Minified input like `{a{x}b}` still parses.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/10260
