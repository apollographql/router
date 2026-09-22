### Fix connector mappings silently misreading names that begin with a keyword or a number ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

`JSONSelection` could stop reading a name part of the way through and treat the remainder as a separate selection. Two forms of this existed, both with the same cause and the same worst case.

**Names beginning with `null`, `true`, or `false`.** The keywords were matched without checking for a word boundary, so `null` consumed the first four characters of `nullableName`. In an alias position the result was not an error but a wrong answer: `displayName: nullableName` parsed as `displayName: null ableName`, assigning the literal `null` to `displayName` and quietly adding a second selection for a field called `ableName`. Elsewhere the same mis-parse surfaced as an opaque `nom::error::ErrorKind::Eof`, rejecting expressions such as `$(nullField ?? "fallback")` and `alias: [falsePositives]`.

**Paths rooted at a number.** A numeric literal may have a fractional part with no digits, so `1.` claimed the `.` that the path step in `1.foo` needed. `alias: 1.foo` parsed as `alias: 1.0 foo`, again two selections where one was written, and `$(1.foo)` failed to parse. `1.5.foo` was always correct, which is what showed this to be a gap rather than the intended rule.

If you have a connector whose mapping aliases a field name starting with `null`, `true`, or `false`, or aliases a path rooted at a number, this release changes its output from `null` to the intended value. It is worth checking whether anything downstream came to depend on the null.

Both keywords and numeric literals now stop at a boundary: a keyword is a literal only where the next character could not have continued an identifier, and a digitless fractional part is only taken where no key could follow. Bare keys such as `nullableName` on its own, `outer { nullableName }`, or `nullableName: renamed` were never affected, because those are parsed as keys rather than as literal expressions.

Separately, a selection list no longer accepts two items that abut with an identifier character on each side and nothing between them, since that can only arise from a name the parser stopped reading early. Input like `alias: 1b: 2`, which previously parsed as two selections, is now reported as an error. Whitespace-free input remains valid wherever the items are genuinely distinct tokens, so `{a{x}b}` still parses.

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/PULL_NUMBER
