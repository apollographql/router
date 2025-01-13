### Fix missing Content-Length header in subgraph requests ([Issue #6503](https://github.com/apollographql/router/issues/6503))

A new telemetry feature introduced in the Router version 1.59.0 would convert all request bodies to subgraphs into stream to infer the total body sizes. This would cause requests to subgraphs to no longer have a `Content-Length` header, which could cause issues with some GraphQL servers.

This solves this issue by using `SizeHint` when possible to infer the body size instead.

By [@nmoutschen](https://github.com/nmoutschen) in https://github.com/apollographql/router/pull/6538
