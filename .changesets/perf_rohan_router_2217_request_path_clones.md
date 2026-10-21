### Stop copying the response body, query and variables on every request ([PR #10551](https://github.com/apollographql/router/pull/10551))

The router copied each non-streamed response body after serializing it, even when no telemetry needed it. It now copies the body only when the `router.response` event, or a router selector that reads the response body or its errors (`response_body`, `response_errors`, `response_errors_count` or `response_errors_field`), is configured.

The router also no longer copies the query on every request in the automatic persisted queries (APQ) layer, and the client's variables on every supergraph and execution request. Responses, APQ behaviour and telemetry output are unchanged.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10551
