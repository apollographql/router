### fix(telemetry): do not display otel bug if the trace is sampled ([PR #3832](https://github.com/apollographql/router/pull/3832))

We changed the way we are sampling spans. If you had logs in an evicted span it displays that log `Unable to find OtelData in extensions; this is a bug` which is incorrect, it's not a bug, we just can't display the `trace_id` because there is no `trace_id` as the span has not been sampled.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3832