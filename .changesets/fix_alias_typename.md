### Support bare top-level `__typename` when aliased ([Issue #2792](https://github.com/apollographql/router/issues/2792))

PR #1762 implemented support for the query `{ __typename }` but it did not work properly if the top-level standalone `__typename` field was aliased. This now works properly.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/2791
