### Strip dashes from trace_id in CustomTraceIdPropagator ([Issue #4892](https://github.com/apollographql/router/issues/4892))

Trace ID in opentelemetry is represented as a 128 bit number. This is usually represented as a hex string without dashes and is based on w3c trace id format.

If for example, raw trace id from headers are represented by uuid4 and contains dashes, those dashes should be stripped so the raw trace id value can be parsed into a valid `trace_id`.


By [@kindermax](https://github.com/kindermax) in https://github.com/apollographql/router/pull/5071