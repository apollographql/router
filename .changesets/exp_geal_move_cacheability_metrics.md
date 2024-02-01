### Move cacheability metrics to the entity cache plugin ([Issue #4253](https://github.com/apollographql/router/issues/4253))

The metric was generated in the telemetry plugin before, but it was not very convenient to keep it there. This adds more configuration:
- enable or disable the metrics
- set the metrics storage TTL (default is 60s)
- the metric's typename attribute is disabled by default. Activating it can greatly increase the cardinality

This also includes some cleanup and performance improvements

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4469