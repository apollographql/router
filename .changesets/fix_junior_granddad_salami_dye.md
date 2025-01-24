### Fix missing Content-Length header in subgraph requests ([Issue #6503](https://github.com/apollographql/router/issues/6503))

A change in `1.59.0` caused the Router to send requests to subgraphs without a `Content-Length` header, which would cause issues with some GraphQL servers that depend on that header.

This solves the underlying bug and reintroduces the `Content-Length` header.

By [@nmoutschen](https://github.com/nmoutschen) in https://github.com/apollographql/router/pull/6538
