### add events tracking subgraph requests retry and break ([Issue #2518](https://github.com/apollographql/router/issues/2518)), ([Issue #2736](https://github.com/apollographql/router/issues/2736))

New metrics tracking retries:
- `apollo_router_subgraph_request_retry_break_count`
- `apollo_router_subgraph_request_retry_attempt_count`

New spans:
- `receive_body` tracking the time spent receiving the request body (debug level)

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2829