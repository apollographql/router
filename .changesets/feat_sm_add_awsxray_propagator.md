Adds support for the OpenTelemetry AWS X-Ray tracing propagator.

This propagator helps propagate tracing information from upstream services (such as AWS load balancers) to downstream services and handles conversion between the X-Ray trace id format and OpenTelemetry span contexts.

By [@scottmace](https://github.com/scottmace) in https://github.com/apollographql/router/pull/3580
