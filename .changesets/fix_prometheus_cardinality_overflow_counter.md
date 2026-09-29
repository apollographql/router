### Report `apollo.router.telemetry.metrics.cardinality_overflow` on the Prometheus endpoint

Since v2.13.0, routers exporting metrics only through Prometheus never reported `apollo.router.telemetry.metrics.cardinality_overflow`, even when a metric went past its cardinality limit. OTLP exports were not affected. The Prometheus endpoint now reports the counter again.

The counter behaves the same way on every exporter:

- It carries a `metric.name` attribute with the OpenTelemetry name of the overflowed metric, for example `metric_name="http.server.request.duration"` on Prometheus. Before v2.13.0 the counter had no attributes.
- It goes up by one per Prometheus scrape (or per export, without Prometheus) while the metric has overflowed series, rather than once per overflow warning as it did before v2.13.0. Alerts on any increase keep working; alerts on a fixed value may need adjusting.
- When OTLP and Prometheus are both enabled, overflow is counted once, on each Prometheus scrape, and OTLP exports the same single series. Since v2.13.0 OTLP counted it on each export instead, so the counter now rises at the scrape rate, and doesn't rise while nothing scrapes the endpoint. Counting from Prometheus's cumulative state also reports overflow that an OTLP exporter using `temporality: delta` never reaches within one export interval.

By [@bryncooke](https://github.com/bryncooke)
