### Report `apollo.router.telemetry.metrics.cardinality_overflow` on the Prometheus endpoint, once per start of overflow

Since v2.13.0, routers exporting metrics only through Prometheus never reported `apollo.router.telemetry.metrics.cardinality_overflow`, even when a metric went past its cardinality limit. The Prometheus endpoint now reports it again.

The counter now goes up once when a metric starts overflowing, and not again while it stays overflowed, so a steady overflow no longer counts once per export, scrape or scraper. Cumulative sums and histograms keep an overflow until the router reloads, so each counts once. Observable gauges reflect each collection, and delta temporality starts each export interval empty, so a metric can stop overflowing and later start again; each observed restart counts again.

- **OTLP, with or without Prometheus:** the OTLP exporter counts, with the OpenTelemetry metric name, for example `metric.name="http.server.request.duration"`. From v2.13.0 to v2.16.x it went up on every export while a metric was overflowed, so its rate changes. Alerts on any increase keep working; alerts on a rate or a fixed value may need adjusting.
- **Prometheus only:** scrapes count, labelled with the Prometheus family name, for example `metric_name="http_server_request_duration_seconds"`.
- **Apollo usage reporting** is unchanged: it counts every export in which a metric has overflowed.

In v2.12.0 and earlier, the counter had no attributes and counted the OpenTelemetry SDK's overflow warnings. That was once when a counter's overflow bucket was created, but once per measurement with a new attribute set for histograms and gauges.

By [@bryncooke](https://github.com/bryncooke)
