### Native query planner now receives both "plan" and "path" limits configuration ([PR #6316](https://github.com/apollographql/router/pull/6316))

The native query planner now correctly sets two experimental configuration options for limiting query planning complexity.  These were previously available in the configuration and observed by the legacy planner, but were not being passed to the new native planner until now:

- `supergraph.query_planning.experimental_plans_limit`
- `supergraph.query_planning.experimental_paths_limit`

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6316