### Fix and test experimental_retry ([PR #6338](https://github.com/apollographql/router/pull/6338))

Fix the behavior of `experimental_retry` and make sure both the feature and metrics are working.
An entry in the context was also added, which would be useful later to implement a new standard attribute and selector for advanced telemetry.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/6338