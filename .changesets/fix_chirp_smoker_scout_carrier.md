### Return HTTP 400 and a graphql error when registering an APQ operation with the wrong hash ([Issue #2948](https://github.com/apollographql/router/issues/2948))

Previously, when a client tried to persist a query with the wrong hash, we would log an error, and execute the query (without inserting the query into the APQ cache).
We now return a GraphQL error and don't execute the query.

This change reflects the gateway's behavior.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/3128
