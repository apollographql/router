### fix: propagate evaluated plans limit and paths limit to the native query planner ([PR #6316](https://github.com/apollographql/router/pull/6316))

Two experimental query planning complexity limiting options now work with the native query planner:
- `supergraph.query_planning.experimental_plans_limit`
- `supergraph.query_planning.experimental_paths_limit`

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6316