### Add support for downloading supergraph schema from a list of URLs. ([Issue #4219](https://github.com/apollographql/router/issues/4219))

`APOLLO_ROUTER_SUPERGRAPH_URLS` takes a comma separated list of URLs which will be polled in order to try and retrieve the supergraph schema.

This is useful for users who require supergraph deployments to be synchronized via gitops workflow.
Poll interval is controlled by `APOLLO_UPLINK_POLL_INTERVAL` and defaults to 10s.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4377
