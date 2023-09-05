### Do not record a trace if telemetry is not configured

The OpenTelemetry handling code had a constant overhead on every request, due to the OpenTelemetryLayer recording data for every span, even when telemetry is not actually set up. We introduce a sampling filter that disables it entirely when no exporters are configured, which provides a performance boost in basic setups.
It also provides performance gains when exporters are set up: if a sampling ratio or client defined sampling are used, then the filter will only send the sampled traces to the rest of the stack, thus reducing the overhead again.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2999
