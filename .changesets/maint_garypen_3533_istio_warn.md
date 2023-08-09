### Add a warning if we think istio-proxy injection is causing problems ([Issue #3533](https://github.com/apollographql/router/issues/3533))

We have encountered situations where the injection of istio-proxy in a router pod (executing in Kubernetes) causes networking errors during uplink retrieval.

The root cause is that the router is executing and attempting to retrieve uplink schemas while the istio-proxy is simultaneously modifying network configuration.

This new warning message will direct users to information which should help them to configure their kubernetes cluster or pod to avoid this problem.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3545