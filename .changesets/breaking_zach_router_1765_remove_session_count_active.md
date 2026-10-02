### Remove deprecated `apollo.router.session.count.active` metric

The `apollo.router.session.count.active` up/down counter — deprecated throughout 2.x — has been removed. It measured the number of in-flight GraphQL requests, duplicating the OpenTelemetry-compliant `http.server.active_requests` metric and causing confusion in dashboards that displayed both.

If your dashboards or alerts reference `apollo.router.session.count.active` (exported as `apollo_router_session_count_active` in Prometheus), switch to [`http.server.active_requests`](https://www.apollographql.com/docs/graphos/routing/observability/router-telemetry-otel/enabling-telemetry/instruments#opentelemetry-standard-instruments), which follows OpenTelemetry semantic conventions and measures the same value.

By [@BobaFetters](https://github.com/BobaFetters) in https://github.com/apollographql/router/pull/ROUTER-1765
