### Pin transitive `h2` dependency at minimum v0.4.13 to pick up critical flow-control, deadlock, and tracing fixes ([PR #9033](https://github.com/apollographql/router/pull/9033))

`h2` 0.4.13 (released January 5, 2026) contains three fixes directly relevant to the router, which uses h2 exclusively as a client when connecting to subgraphs:

- **Capacity deadlock under concurrent streams ([#860](https://github.com/hyperium/h2/pull/860)) — high relevance:** Under concurrent load with `max_concurrent_streams` limits in effect, flow-control capacity could be assigned to streams still in `pending_open` state. Those streams could never consume the capacity, starving already-open streams and permanently freezing all outgoing traffic on the connection with no error surfaced. This is directly triggerable in the router: any subgraph behind Envoy or a gRPC backend advertises a `max_concurrent_streams` limit (Envoy defaults to 100), and under production load the router will routinely queue more concurrent requests than that limit allows.

- **OTel tracing span lifetime leak ([#868](https://github.com/hyperium/h2/pull/868)) — high relevance:** The h2 `Connection` object captured the active tracing span at connection creation time as its parent, keeping that span alive for the entire lifetime of the connection. Since the router wraps every subgraph request in an OpenTelemetry span and connections are pooled, affected spans could linger indefinitely under sustained traffic — never being exported to the tracing backend and accumulating in memory.

- **Flow-control stall on padded DATA frames ([#869](https://github.com/hyperium/h2/pull/869)) — lower relevance for typical subgraphs, higher for connectors:** Padding bytes in `DATA` frames were not being returned to the flow-control window, causing the connection window to drain to zero and permanently stalling downloads with no error. Typical GraphQL/gRPC subgraphs do not send padded frames, but router connectors calling arbitrary HTTP APIs (e.g., Google Cloud Storage or CDN-backed endpoints) can encounter this.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/9033
