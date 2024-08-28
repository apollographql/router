### Enable native query planner to run in the background ([PR #5790](https://github.com/apollographql/router/pull/5790), [PR #5811](https://github.com/apollographql/router/pull/5811), [PR #5771](https://github.com/apollographql/router/pull/5771), [PR #5860](https://github.com/apollographql/router/pull/5860))

The router now schedules background jobs to run the native (Rust) query planner to compare its results to the legacy implementation. This helps ascertain its correctness before making a decision to switch entirely to it from the legacy query planner.

The router continues to use the legacy query planner to plan and execute
operations, so there is no effect on the hot path. 

To disable running background comparisons with the native query planner, you can configure the router to enable only the `legacy` query planner:

```yaml
experimental_query_planner_mode: legacy
```

By [SimonSapin](https://github.com/SimonSapin) in ([PR #5790](https://github.com/apollographql/router/pull/5790), [PR #5811](https://github.com/apollographql/router/pull/5811), [PR #5771](https://github.com/apollographql/router/pull/5771) [PR #5860](https://github.com/apollographql/router/pull/5860))