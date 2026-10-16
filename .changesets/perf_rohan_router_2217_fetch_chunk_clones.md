### Stop copying request variables on every subgraph fetch and response chunk ([PR #PULL_NUMBER](https://github.com/apollographql/router/pull/PULL_NUMBER))

The router made a full copy of an operation's variables for every subgraph fetch, including the entity `representations` it sends to subgraphs, and kept that copy until the subgraph responded. It only used the copy for a trace-level log line. The router now makes that copy only while the `apollo_router::query_planner::log` target is logging at `trace`.

The router also copied the client's variables once or twice for each response chunk while formatting the response. It now copies them only when the operation declares default values to merge in. Responses and log output are unchanged.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/PULL_NUMBER
