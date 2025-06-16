### Set a valid GraphQL response for websocket handshake response ([PR #7680](https://github.com/apollographql/router/pull/7680))

Since this [PR](https://github.com/apollographql/router/pull/7141) we added more checks on graphql response returned by coprocessors to be compliant with GraphQL specs. When it's a subscription using websocket it was not returning any data and so was not a correct GraphQL response payload. This is a fix to always return valid GraphQL response when doing the websocket handshake.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7680