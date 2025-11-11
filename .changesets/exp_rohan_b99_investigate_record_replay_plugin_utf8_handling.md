### Prevent panic when record/replay plugin encounters non-UTF-8 header values ([PR #8485](https://github.com/apollographql/router/pull/8485))

The record/replay plugin no longer panics when externalizing headers with invalid UTF-8 values. Instead, the plugin writes the header keys and errors to a `header_errors` object for both requests and responses.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8485
