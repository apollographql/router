### Check a type-conditioned path key against the selected value when it is applied over a list ([PR #10370](https://github.com/apollographql/router/pull/10370))

A query plan path key with a type condition, such as `child|[Book]`, matches when the value at `child` is a `Book`. When the same key was applied across a list, the router instead required each list element (the parent of `child`) to be a `Book` before checking `child` itself, so matching values under differently-typed parents were skipped. The type condition is now checked only against the selected value, as it is for a single object.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10370
