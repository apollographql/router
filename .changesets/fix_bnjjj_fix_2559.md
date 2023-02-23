### fix: force ordering of headers plugin ([Issue #2559](https://github.com/apollographql/router/issues/2559))

Force ordering of headers plugin to have better interaction with other plugins.
It gives you the ability to combine the usage of `telemetry` custom attributes coming from headers for metrics and the `headers` plugin to propagate/insert headers to subgraphs.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2670
