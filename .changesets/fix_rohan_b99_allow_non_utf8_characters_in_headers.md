### Log warning instead of returning error for non-UTF-8 headers in `externalize_header_map` ([PR #8828](https://github.com/apollographql/router/pull/8828))

- The router now emits a warning log with the name of the header instead of returning an error.
- The remaining valid headers are returned, which is more consistent with the router's default behavior when a coprocessor isn't used.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8828
