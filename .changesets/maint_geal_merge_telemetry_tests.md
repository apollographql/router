### merge telemetry tests to reduce linking time ([PR #3272](https://github.com/apollographql/router/pull/3272))

We have multiple test executable performing similar tasks just to check configuration. Each of them needs to link an entire router. By moving all of them under the same file we can reduce the time spent in CI

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3272