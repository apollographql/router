### Limit the amount of GraphQL validation errors returned per response ([PR #6187](https://github.com/apollographql/router/pull/6187))

When an invalid query is submitted, the router now returns at most one hundred GraphQL parsing and validation errors in a response. This prevents generating too large of a response for a nonsensical document.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6187