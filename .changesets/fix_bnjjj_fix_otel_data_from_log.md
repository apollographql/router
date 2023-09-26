### fix(telemetry): do not display otel bug if the trace is sampled. ([PR #3832](https://github.com/apollographql/router/pull/3832))

We changed the way we are sampling spans. If you had logs in an evicted span it displays an error: `Unable to find OtelData in extensions; this is a bug`. This is incorrect, as it's not a bug. The logs cannot display the `trace_id` as there is no `trace_id` when the span has not been yet sampled.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3832