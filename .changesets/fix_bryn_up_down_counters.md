### Fix OTLP metrics export to prevent UpDown counter drift ([PR #8174](https://github.com/apollographql/router/pull/8174))

Previously, when using OTLP metrics export with delta temporality configured, UpDown counters could exhibit drift issues where the counter values would become inaccurate over time. This happened because UpDown counters were incorrectly exported as deltas instead of cumulative values.

UpDownCounters will now always be exported as aggregate values as per the otel spec.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8174
