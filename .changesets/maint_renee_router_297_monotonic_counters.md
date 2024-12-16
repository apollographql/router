### Deprecate various metrics ([PR #6350](https://github.com/apollographql/router/pull/6350))

Several metrics have been deprecated in this release, in favor of OpenTelemetry-compatible alternatives:

- `apollo_router_deduplicated_subscriptions_total` - use the `apollo.router.operations.subscriptions` metric's `subscriptions.deduplicated` attribute.
- `apollo_authentication_failure_count` - use the `apollo.router.operations.authentication.jwt` metric's `authentication.jwt.failed` attribute.
- `apollo_authentication_success_count` - use the `apollo.router.operations.authentication.jwt` metric instead. If the `authentication.jwt.failed` attribute is *absent* or `false`, the authentication succeeded.
- `apollo_require_authentication_failure_count` - use the `http.server.request.duration` metric's `http.response.status_code` attribute. Requests with authentication failures have HTTP status code 401.
- `apollo_router_timeout` - this metric conflates timed-out requests from client to the router, and requests from the router to subgraphs. Timed-out requests have HTTP status code 504. Use the `http.response.status_code` attribute on the `http.server.request.duration` metric to identify timed-out router requests, and the same attribute on the `http.client.request.duration` metric to identify timed-out subgraph requests.

The deprecated metrics will continue to work in the 1.x release line.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6350
