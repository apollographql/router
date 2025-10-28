### Add error to recording JSON file instead of panic when non UTF-8 characters found in header value in record/replay plugin ([PR #8485](https://github.com/apollographql/router/pull/8485))

When externalizing headers in the record/replay plugin, headers with invalid values will now have their keys and the error written to an object called `header_errors` for both requests and responses instead of panicking.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8485
