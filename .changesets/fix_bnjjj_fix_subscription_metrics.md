### Fix the error count for subscription requests for apollo telemetry ([PR #3500](https://github.com/apollographql/router/pull/3500))

Count subscription requests only if the feature is enabled.

The router would previously count subscription requests regardless of whether the feature is enabled or not. This changeset will only count subscription requests if the feature has been enabled.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3500
