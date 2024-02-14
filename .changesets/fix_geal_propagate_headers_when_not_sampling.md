### Propagate tracing headers even when not sampling a trace ([Issue #4544](https://github.com/apollographql/router/issues/4544))

When the router was configured to sample only a portion of the trace, either through a ratio or using parent based sampling, and when trace propagation was configured, if a trace was not sampled, the router did not send the propagation headers to the subgraph. The subgraph was then unable to decide whether to record the trace or not. Now we make sure that trace headers will be sent even when a trace is not sampled.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4609