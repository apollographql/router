### Limit the amount of GraphQL validation errors returned in the response ([PR #6187](https://github.com/apollographql/router/pull/6187))

When an invalid query is submitted, the router now returns at most 100 GraphQL parsing and validation errors in the response.
This prevents generating a very large response for nonsense documents.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6187