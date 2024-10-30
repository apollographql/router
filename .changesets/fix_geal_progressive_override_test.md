### Progressive override: fix query planner cache warmup ([PR #6108](https://github.com/apollographql/router/pull/6108))

This fixes an issue in progressive override where the override labels would not be transmitted to the query planner during cache warmup, resulting in queries correctly using the overridden fields at first, but after an update, would revert to non overridden fields, and could not recover.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/6108