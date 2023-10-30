### Authorization: dry run option ([Issue #3843](https://github.com/apollographql/router/issues/3843))

It is now possible to execute authorization directives without modifying the query, but still return the list of affected paths as top-level errors in the response. This allows testing authorization without breaking existing traffic.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4079