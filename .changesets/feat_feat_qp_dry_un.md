### feat: query planner dry-run option ([PR #6656](https://github.com/apollographql/router/pull/6656))

This PR adds a new `dry-run` option to the `Apollo-Expose-Query-Plan` header value that emits the query plans back to Studio for visualizations. This new value will *only* emit the query plan, and abort execution. This can be helpful for tools like `rover`, where query plan generation is needed but not full runtime, or for potentially prewarming query plan caches out of band.

By [@aaronArinder](https://github.com/aaronArinder) and [@lennyburdette](https://github.com/lennyburdette) in https://github.com/apollographql/router/pull/6656.
