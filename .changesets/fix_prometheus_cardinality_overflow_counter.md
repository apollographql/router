### Report `apollo.router.telemetry.metrics.cardinality_overflow` on the Prometheus endpoint

Since v2.13.0, routers exporting metrics only through Prometheus never reported `apollo.router.telemetry.metrics.cardinality_overflow`, even when a metric went past its cardinality limit. OTLP exports were not affected. The Prometheus endpoint now reports the counter again.

The counter behaves the same way on every exporter:

- It carries a `metric.name` attribute with the OpenTelemetry name of the overflowed metric, for example `metric_name="http.server.request.duration"` on Prometheus. Before v2.13.0 the counter had no attributes.
- It goes up by one per export or Prometheus scrape while the metric has overflowed series, rather than once per overflow warning as it did before v2.13.0. Alerts on any increase keep working; alerts on a fixed value may need adjusting.
- When OTLP and Prometheus are both enabled, overflow is counted once, by the OTLP exporter, and the Prometheus endpoint shows the same single series.

By [@bryncooke](https://github.com/bryncooke)
