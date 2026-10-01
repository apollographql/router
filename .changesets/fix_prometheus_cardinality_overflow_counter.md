### Report `apollo.router.telemetry.metrics.cardinality_overflow` on Prometheus again

Since v2.13.0, routers that export metrics only through Prometheus never showed `apollo.router.telemetry.metrics.cardinality_overflow`, even when a metric went over its cardinality limit. The counter now appears again.

The counter now goes up once when a metric goes over its limit, not on every export or scrape. It goes up again only if the metric drops back under the limit and later goes over again.

The label that names the overflowing metric (`metric.name`, shown as `metric_name` in Prometheus) depends on your exporters:

- **Prometheus only:** the Prometheus metric name, for example `http_server_request_duration_seconds`.
- **With OTLP:** the OpenTelemetry metric name, as before, for example `http.server.request.duration`.

If you use OTLP, the counter now rises more slowly than in v2.13.0–v2.16.x, which counted on every export. Alerts on any increase still work; alerts on a rate or a fixed value may need adjusting. Apollo usage reporting is unchanged.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10309
