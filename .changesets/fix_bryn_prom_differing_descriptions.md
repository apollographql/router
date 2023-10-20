### Make all apollo_router_http_requests_total metrics match ([Issue #4047](https://github.com/apollographql/router/issues/4047))

The Router emitted identical metrics events with different descriptions causing an error to be emitted to the logs.

```log
OpenTelemetry metric error occurred: Metrics error: Instrument description conflict, using existing.
```

The metrics description have been brought into alignment.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4065
