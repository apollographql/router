### Fix OTLP metrics export to prevent UpDown counter drift ([PR #8174](https://github.com/apollographql/router/pull/8174))

Previously, when using OTLP metrics export with delta temporality configured, UpDown counters could exhibit drift issues where the counter values would become inaccurate over time. This happened because UpDown counters were incorrectly exported as deltas instead of cumulative values.

**What you might have experienced:**

- Inaccurate counter values in your monitoring dashboards for metrics that can go both up and down
- Counter values that didn't reflect the true current state
- Counter drift that accumulated over time, especially in long-running router instances

**What's fixed:**
- UpDown counters now always use cumulative temporality regardless of your OTLP temporality configuration
- This ensures UpDown counters maintain accurate values and prevent drift
- Other metric types (regular counters, histograms, gauges) continue to respect your configured temporality setting

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8174
