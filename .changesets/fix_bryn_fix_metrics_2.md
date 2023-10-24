### Ensure `apollo_router_http_requests_total` metrics match ([Issue #4047](https://github.com/apollographql/router/issues/4047))
Identically _named_ metrics were being emitted for `apollo_router_http_requests_total` (as intended) but with different _descriptions_ (not intended) resulting in occasional, but noisy, log warnings:
```
OpenTelemetry metric error occurred: Metrics error: Instrument description conflict, using existing.
```
The metrics' descriptions have been brought into alignment to resolve the log warnings and we will follow-up with additional work to think holistically about a more durable pattern that will prevent this from occurring in the future.
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/4089
