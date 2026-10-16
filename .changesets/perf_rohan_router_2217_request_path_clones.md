### Stop copying the response body, query and variables on every request ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

The router copied each non-streamed response body after serializing it, even when no telemetry needed it. It now copies the body only when the `router.response` event or a `response_body` router selector is configured.

The router also stopped copying the query on every request in the automatic persisted queries (APQ) layer, and the client's variables on every supergraph and execution request. Responses, APQ behaviour and telemetry output are unchanged.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
