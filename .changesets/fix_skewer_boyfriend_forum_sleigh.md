### Update `apollo-compiler` for two small improvements ([PR #5347](https://github.com/apollographql/router/pull/5347))

Updated our underlying `apollo-rs` dependency on our `apollo-compiler` crate to bring in two nice improvements:

- **Fix validation performance bug**

  Adds a cache in fragment spread validation, fixing a situation where validating a query
  with many fragment spreads against a schema with many interfaces could take multiple
  seconds to validate.

- **Remove ariadne byte/char mapping**

  Generating JSON or CLI reports for apollo-compiler diagnostics used a translation layer
  between byte offsets and character offsets, which cost some computation and memory
  proportional to the size of the source text. The latest version of `ariadne` allows us to
  remove this translation.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/5347