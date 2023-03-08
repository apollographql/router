### Document the `Context::get` properly ([Issue #2580](https://github.com/apollographql/router/issues/2580))

Bad documentation for `Context::get`, if we have an error it doesn't mean the context entry didn't exist, it's a deserialize error.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2669
