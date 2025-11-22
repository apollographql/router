### Fix missing Content-Length header  ([Issue](https://github.com/apollographql/router/issues/7941))

Apollo Router was using `transfer-encoding: chunked` for GraphQL responses with known content lengths instead of the more efficient content-length header due to the fleet_detector plugin destroying HTTP body size hints when collecting metrics.

In https://github.com/apollographql/router/pull/6538 we solved this same issue for metrics for the `router -> subgraph` by modifying the fleet_detector plugin to preserve size hints for bodies with known content lengths by checking `size_hint.exact()` and only wrapping unknown-size bodies in streams for byte counting. This PR extends the existing fix already applied to `router → subgraph` requests to also cover `client → router` requests/responses, ensuring size hints flow correctly through the entire pipeline for optimal HTTP header selection.

By [@morriswchris](https://github.com/morriswchris) in https://github.com/apollographql/router/pull/7977