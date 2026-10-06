### `cardinality_overflow` counts once per overflow with OTLP

With OTLP, `apollo.router.telemetry.metrics.cardinality_overflow` now goes up once when a metric starts overflowing, not on every export while it stays over its limit. Alerts based on `rate()` or `increase()`, or on a fixed value, may need adjusting. GraphOS usage reporting is unchanged.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/10309
