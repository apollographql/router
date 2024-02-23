### Prioritize errors in GraphQL response ([Issue #4375](https://github.com/apollographql/router/issues/4375))

Previously, the router would return data from an operation before any potential errors in the request.
[As recommended in the GraphQL spec](https://spec.graphql.org/draft/#note-6f005), the suggested route
is to try to return errors first before data in the response. The router now does so.

By [@nicholascioli](https://github.com/nicholascioli) in https://github.com/apollographql/router/pull/4728
