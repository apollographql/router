### Tracing: add child spans to the existing `query_planning` span ([PR #7123](https://github.com/apollographql/router/pull/7123)) 

There are several processes happening before an operation is being sent to the query planner for planning. The existing `query_planning` span encapsulates all of it without a detailed breakdown:
```
query myQuery
│  parse_query
└─── supergraph
│    └─── query_planning
└───execute
```


With this change, we are adding more details to `query_planning` span to have a fuller story in the traces around query planning. There are now three possible children spans: `cache_lookup`, `process_and_plan` and `waiting_for_cache`.

**Cache Lookup**. The following span structure is expected if the requested operation is already in the cache and has been planned:

    ```
    query myQuery
    │  parse_query
    └─── supergraph
    │    └─── query_planning
    │         └─── cache_lookup
    └───execute
    ```
**Process and Plan**. The following span structure is expected if the requested operation is not in the cache, and we need to do planning:
    ```
    query myQuery
    │  parse_query
    └─── supergraph
    │    └─── query_planning
    │         │    cache_lookup
    │         └─── process_and_plan
    │              └─── worker_pool
    │                   └─── plan
    └───execute
    ```
**Waiting for cache**. The following span structure is expected if the requested operation is not in the cache, **but a different connection is already planning it**, making the current connection wait for that result to be written to cache before using it:

    ```
    query myQuery
    │  parse_query
    └─── supergraph
    │    └─── query_planning
    │         │    cache_lookup
    │         └─── waiting_for_cache
    └───execute
    ```

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/7123