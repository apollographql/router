### Helm update to allow a list of gateways to VirtualService ([Issue #4464](https://github.com/apollographql/router/issues/4464))

Configuration of the router's Helm chart has been updated to allow multiple gateways. This enables configuration of multiple gateways in Istio VirtualService.

The previous configuration for a single `virtualservice.gatewayName` has been deprecated in favor of a configuration for an array of `virtualservice.gatewayNames`.

By [@marcantoine-bibeau](https://github.com/marcantoine-bibeau) in https://github.com/apollographql/router/pull/4520
