### Printed connector mappings preserve tabs, carriage returns, and other control characters in strings

When a connector mapping was printed (for example, the selection shown in the connectors debugger), string literals and quoted keys were escaped the way JSON escapes them, producing `\t`, `\r`, or `\u0000`. The mapping language only understands `\n` and backslash-escaped quotes, so the printed text no longer meant the same thing: `"\t"` reads back as the letter `t`.

Strings are now printed using only escapes the mapping language reads back (`\"`, `\\`, and `\n`), with other characters written as-is, so printing a mapping and parsing it again gives the same values.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10364
