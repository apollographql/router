### small performance improvements for telemetry ([PR #3656](https://github.com/apollographql/router/pull/3656))

The SpanMetricsExporter, used to report span timings hade a few inefficiencies in the way it recognized spans, and it brought a constant overhead to the router usage, even when telemetry was not configured. It has now been isolated and optimized

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3656