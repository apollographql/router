### Strip dashes from `trace_id` in `CustomTraceIdPropagator` ([Issue #4892](https://github.com/apollographql/router/issues/4892))


The router now strips dashes from trace IDs to ensure conformance with OpenTelemetry.

In OpenTelemetry, trace IDs are 128-bit values represented as hex strings without dashes, and they're based on W3C's trace ID format.

This has been applied within the router to `trace_id` in `CustomTraceIdPropagator`.

Note, if raw trace IDs from headers are represented by uuid4 and contain dashes, the dashes should be stripped so that the raw trace ID value can be parsed into a valid `trace_id`.


By [@kindermax](https://github.com/kindermax) in https://github.com/apollographql/router/pull/5071