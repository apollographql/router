### Add a private part to the Context structure ([Issue #2800](https://github.com/apollographql/router/issues/2800))

There's a cost in using the `Context` structure throughout a request's lifecycle, due to the JSON serialization and deserialization, so it should be reserved from inter plugin communication between rhai, coprocessor and Rust. But for internal router usage, we can have a more efficient structure that avoids serialization costs, and does not expose data that should not be modified by plugins.

That structure is based on a map indexed by type id, which means that if some part of the code can see that type, then it can access it in the map.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2802