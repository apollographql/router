### Export metrics emitted by the router's QoS layers ([Issue #ROUTER-2056](https://github.com/apollographql/router/issues/ROUTER-2056))

Metrics emitted by the router's QoS layers now reach configured metrics exporters. Previously they were silently dropped.

These layers are libraries, so following the OpenTelemetry specification they create their instruments from the global meter provider rather than being handed one. The router configures its own metrics pipeline and never populated that global, leaving it as OpenTelemetry's default no-op provider — so any instruments from these libraries were created successfully, recorded on every request, and went nowhere.

The router now routes the global meter provider at its own, so this exports alongside every other router metric, on every exporter. It starts appearing where it previously did not. Tracing was never affected: spans from these layers were already exported, because the global *tracer* provider was being set.

As with the tracer provider, the router only claims the global meter provider when it owns the process-wide telemetry setup. Custom routers built on the `apollo-router` library keep whichever meter provider they installed themselves.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9974
