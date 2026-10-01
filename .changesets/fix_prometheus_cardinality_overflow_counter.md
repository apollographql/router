### Report `apollo.router.telemetry.metrics.cardinality_overflow` on Prometheus again

Each router metric has a cardinality limit: the most distinct attribute combinations it keeps. When a metric goes over its limit, the router reports it on the `apollo.router.telemetry.metrics.cardinality_overflow` counter. Since v2.13.0, routers that export metrics only through Prometheus never showed this counter. It now appears again.

The counter goes up by one when a metric first goes over its limit, and not again while that metric stays over it. If the metric later drops back under its limit and goes over it again, the counter goes up again.

The counter's `metric.name` attribute (shown as `metric_name` in Prometheus) names the metric that went over its limit. How that name is written depends on your exporters:

- **Prometheus only:** the metric's Prometheus name, for example `http_server_request_duration_seconds`.
- **With OTLP:** the metric's OpenTelemetry name, as in earlier versions, for example `http.server.request.duration`.

If you use OTLP, the counter now rises more slowly than in v2.13.0–v2.16.x, which counted on every export. Alerts on any increase still work, but alerts on a rate or a fixed value may need adjusting. Apollo usage reporting is unchanged.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10309
