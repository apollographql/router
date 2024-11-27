### Avoid creating stub span for supergraph events if current span exists ([PR #6096](https://github.com/apollographql/router/pull/6096))

The router optimized its telemetry implementation by not creating a redundant span when it already has a span available to use the span's extensions for supergraph events.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6096