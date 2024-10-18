### Don't create a stub span for supergraph events if it already has a current span ([PR #6096](https://github.com/apollographql/router/pull/6096))

Don't create useless span when we already have a span available to use the span's extensions.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6096