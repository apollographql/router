### Try to stop OTLP controllers when Telemetry is dropped ([Issue #3140](https://github.com/apollographql/router/issues/3140))

We already have code to specifically drop tracers and we are adding some additional logic to do the same thing with metrics exporters.

This will improve the transmission of metrics from OTLP controllers when a router is shut down.

fixes: #3140

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3143