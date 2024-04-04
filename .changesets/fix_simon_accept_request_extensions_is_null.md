### Accept `extensions: null` in a GraphQL request ([Issue #3388](https://github.com/apollographql/router/issues/3388))

In GraphQL requests, `extensions` is an optional map.
Passing an explicit `null` was incorrectly considered a parse error.
Now it is equivalent to omiting that field entirely, or to passing an empty map.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/4911
