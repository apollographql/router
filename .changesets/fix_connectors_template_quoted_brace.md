### Connectors URL and header templates accept `{` and `}` inside string literals in expressions

A `{...}` expression in a connector URL template, header value, or `@cacheTag` format ended at the first unmatched `}`, even when that brace was inside a string literal in the expression. A valid template such as `/search?q={$("}")}` or `{$args.id->eq("{")}` failed to parse with an "Unterminated string literal" or "missing closing }" error, both at composition and at router startup.

The end of an expression is now found by skipping over the expression's single- and double-quoted strings (including backslash escapes), so braces inside strings no longer end the expression. Braces that are part of the selection itself, such as `{$args.filter { id }}`, are handled as before.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10363
