### Sample FTV1 by supergraph request instead of by subgraph request ([Issue #2655](https://github.com/apollographql/router/issues/2655))

Because tracing can be costly, it is only enabled for a configurable fraction of requests. Each request is selected for tracing or not with a corresponding probability. This used to be done as part of the subgraph service, meaning that when a single supergraph request handled by the Router involves making multiple subgraph requests, it would be possible (and likely) that tracing would only be enabled for some of those sub-requests. If this same supergraph request is repeated enough times the aggregated metrics should be fine, but for smaller sample size this risks giving an unexpectedly partial view of what’s happening.

Now each supergraph request recieved by the Router is sampled or not for FTV1, and all corresponding subgraph requests reuse the same result.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2656
