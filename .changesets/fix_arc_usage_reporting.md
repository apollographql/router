### Wrap `UsageReporting` in `Arc` to avoid expensive clone ([PR #4731](https://github.com/apollographql/router/pull/4731))

Cloning the `UsageReporting` structure into extensions (https://github.com/apollographql/router/blob/dev/apollo-router/src/query_planner/caching_query_planner.rs#L390) had
a high cost when the map of referenced fields was large.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/4731