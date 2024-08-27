### Enable native (rust) query planner to run in the background ([PR #5790](https://github.com/apollographql/router/pull/5790), [PR #5811](https://github.com/apollographql/router/pull/5811), [PR #5771](https://github.com/apollographql/router/pull/5771), [PR #5860](https://github.com/apollographql/router/pull/5860))

The router now schedules background jobs to run the native query planner in
order to compare its results to the legacy implementation. This is one of the
ways to help us ascertain its correctness before making a decision to switch
entirely to the native planner.

The legacy query planner implementation continues to be used to plan and execute
operations, so there is no effect on the hot path. 

You can disable running background comparisons in the native query planner by
enabling just the `legacy` mode in router.yaml:
```yaml
experimental_query_planner_mode: legacy
```

By [SimonSapin](https://github.com/SimonSapin) in ([PR #5790](https://github.com/apollographql/router/pull/5790), [PR #5811](https://github.com/apollographql/router/pull/5811), [PR #5771](https://github.com/apollographql/router/pull/5771) [PR #5860](https://github.com/apollographql/router/pull/5860))