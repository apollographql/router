### Preserve Content-Length header for responses with known size ([Issue #7941](https://github.com/apollographql/router/issues/7941))

The router now uses the `Content-Length` header for GraphQL responses with known content lengths instead of `transfer-encoding: chunked`. Previously, the `fleet_detector` plugin destroyed HTTP body size hints when collecting metrics.

This extends the fix from [#6538](https://github.com/apollographql/router/pull/6538), which preserved size hints for `router → subgraph` requests, to also cover `client → router` requests and responses. Size hints now flow correctly through the entire pipeline for optimal HTTP header selection.

By [@morriswchris](https://github.com/morriswchris) in https://github.com/apollographql/router/pull/7977