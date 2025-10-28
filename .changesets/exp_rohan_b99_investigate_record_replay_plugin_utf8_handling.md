### Log error instead of panic when non UTF-8 characters found in header value in record/replay plugin ([PR #8485](https://github.com/apollographql/router/pull/8485))

Replaces the `expect` call with an error log in the record/replay plugin when externalizing headers.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8485
