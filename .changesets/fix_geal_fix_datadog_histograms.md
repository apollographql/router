### Fix histograms metrics in OTLP ([Issue #2393](https://github.com/apollographql/router/issues/2493))

with the "inexpensive" metrics selector, histogram are only reported as gauges, and so they will be incorrectly interpreted when reaching Datadog

By [@GEal](https://github.com/geal) in https://github.com/apollographql/router/pull/2564