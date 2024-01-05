### Add support for downloading supergraph schema from a list of URLs ([Issue #4219](https://github.com/apollographql/router/issues/4219))

The `APOLLO_ROUTER_SUPERGRAPH_URLS` environment variable has been introduced to support downloading supergraph schema from a list of URLs. This is useful for users who require supergraph deployments to be synchronized via GitOps workflows.

You configure `APOLLO_ROUTER_SUPERGRAPH_URLS` with a comma separated list of URLs that will be polled in order to try and retrieve the supergraph schema, and you configure the polling interval by setting `APOLLO_UPLINK_POLL_INTERVAL` (default: 10s).


By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4377
