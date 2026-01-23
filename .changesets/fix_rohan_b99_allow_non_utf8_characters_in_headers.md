### Log warning instead of returning error when non-utf8 header passed through externalize_header_map ([PR #8828](https://github.com/apollographql/router/pull/8828))

- A warning log with the name of the header will now be emitted
- The remaining valid headers will be returned now instead of an error, which is more consistent with the router's default behaviour when a coprocessor is not used.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8828