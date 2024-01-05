### Expose query plan and paths limits ([PR #4367](https://github.com/apollographql/router/pull/4367))

Two new configuration options have been added to reduce the impact of complex queries on the planner:

- `experimental_plans_limit` limits the number of generated plans. (Note: already generated plans remain valid, but they may not be optimal.) 

- `experimental_paths_limit` stops the planning process entirely if the number of possible paths for a selection in the schema gets too large.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4367