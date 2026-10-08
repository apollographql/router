### Report `apollo.router.telemetry.metrics.cardinality_overflow` on Prometheus

Each router metric has a cardinality limit: the most distinct attribute combinations it keeps. When a metric goes over its limit, the router reports it on the `apollo.router.telemetry.metrics.cardinality_overflow` counter. Since v2.13.0, routers that export metrics only through Prometheus never showed this counter. Prometheus now reports it, the same way as the other exporters.

The counter goes up by one when a metric first goes over its limit, and not again while that metric stays over it. If the metric later drops back under its limit and goes over it again, the counter goes up again.

The counter's `metric.name` attribute (shown as `metric_name` in Prometheus) is the OpenTelemetry name of the metric that went over its limit, for example `http.server.request.duration`, whichever exporters you use. In v2.12.0 and earlier the counter had no `metric.name` attribute and went up once for each overflow warning, so dashboards and alerts built on v2.12.0 may need adjusting.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10309
