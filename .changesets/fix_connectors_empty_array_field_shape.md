### Connectors validation no longer reports a missing field when a key is mapped over an empty array

Selecting a key from an array maps the key over the array's elements, so `$([]).name` evaluates to `[]` with no errors. Static shape checking treated the empty array as if every element lacked the field and reported `field `name` not found`, so composition rejected valid mappings such as `names: $([]).name` in `@connect(selection:)` or in a URL template. The same happened for empty arrays nested inside other arrays (`[[]]`).

A key mapped over an empty array now has an empty-array shape. Mapping a field that no element of a non-empty array can have is still reported as an error.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10362
