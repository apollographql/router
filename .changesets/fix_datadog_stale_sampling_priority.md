### Fix stale Datadog sampling priority propagating to subgraphs ([PR #9787](https://github.com/apollographql/router/pull/9787))

The router could tell subgraphs to sample a trace that the router itself had decided not to sample, when using the Datadog exporter. An upstream `x-datadog-sampling-priority` header indicating "keep" could survive in the trace's `trace_state` even after the router's own sampler later decided to drop the trace, causing that stale "keep" priority — rather than the router's actual decision — to be forwarded downstream.

This is now fixed: the outgoing `x-datadog-sampling-priority` header always agrees with the span's own sampling decision.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9787
