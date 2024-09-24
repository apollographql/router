### Fix Datadog sample propagation ([PR #6005](https://github.com/apollographql/router/pull/6005))

[PR #5788](https://github.com/apollographql/router/pull/5788) introduced a regression where sampling was set on propagated headers regardless of the sampling decision in the router or upstream.

This PR reverts the code in question and adds a test to check that a non-sampled request doesn't result in sampling in the downstream subgraph service.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6005
