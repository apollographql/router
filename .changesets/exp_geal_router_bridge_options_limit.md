### Expose query plan and paths limits ([PR #4367](https://github.com/apollographql/router/pull/4367))

This introduces two new options, `experimental_plans_limit` and `experimental_paths_limit`, to reduce the impact of complex queries on the planner. `experimental_plans_limit` limits the number of generated plans. Already generated plans will be valid, but may not be optimal. `experimental_paths_limit` stops entirely the planning process if the number of possible paths for a selection in the schema gets too large

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4367