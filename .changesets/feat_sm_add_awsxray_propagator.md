### Adds support for the OpenTelemetry AWS X-Ray tracing propagator. ([PR #3580](https://github.com/apollographql/router/pull/3580))

This propagator helps propagate tracing information from upstream services (such as AWS load balancers) to downstream services. It also handles conversion between the X-Ray trace id format and OpenTelemetry span contexts.

By [@scottmace](https://github.com/scottmace) in https://github.com/apollographql/router/pull/3580
