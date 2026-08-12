### Export metrics emitted by the router's QoS layers ([Issue #ROUTER-2056](https://github.com/apollographql/router/issues/ROUTER-2056))

Metrics emitted by the router's QoS layers now reach configured metrics exporters. Previously they were silently dropped.

These layers are libraries, so following the OpenTelemetry specification they create their instruments from the global meter provider rather than being handed one. The router configures its own metrics pipeline and never populated that global, leaving it as OpenTelemetry's default no-op provider — so the instruments below were created successfully, recorded on every request, and went nowhere:

- `apollo.qos.circuit_breaker.requests` — requests a circuit accepted, rejected, or let through as a probe
- `apollo.qos.circuit_breaker.state` — each circuit's current state (`0` closed, `1` half-open, `2` open)
- `apollo.qos.circuit_breaker.state.transitions` — circuit state changes, with the states moved between
- `apollo.platform.qos.limits.requests` — requests seen by the rate limiting applied under a limited license

The router now routes the global meter provider at its own, so these export alongside every other router metric, on every exporter. Tracing was never affected: spans from these layers were already exported, because the global *tracer* provider was being set.

Circuit breaker metrics are new in this release, so only `apollo.platform.qos.limits.requests` changes behaviour for existing deployments — it starts appearing where it previously did not.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9974
