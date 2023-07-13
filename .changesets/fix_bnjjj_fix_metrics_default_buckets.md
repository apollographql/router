### Enforce default buckets for metrics ([PR #3432](https://github.com/apollographql/router/pull/3432))

When you haven't any `telemetry.metrics.common` configuration the default buckets were wrong and you had no buckets at all. With this fix by default it set these buckets: [0.001, 0.005, 0.015, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 1.0, 5.0, 10.0]

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3432