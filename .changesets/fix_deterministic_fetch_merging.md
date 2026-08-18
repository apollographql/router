### Query planner: fetch merging now produces deterministic plan shapes ([PR #9842](https://github.com/apollographql/router/pull/9842))

Planning the same operation repeatedly on one router could produce different (individually valid) plan shapes across calls: fetch-node merge groups were held in a HashMap-backed multimap and merged in random across-key order per planning call, which downstream construction turned into differing `_entities` type-condition batching. Merge groups are now processed in deterministic topological order, so identical inputs yield byte-identical plans.

By [@martijnwalraven](https://github.com/martijnwalraven) in https://github.com/apollographql/router/pull/9842
