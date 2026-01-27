### Add memory tracking metrics for requests ([PR #8717](https://github.com/apollographql/router/pull/8717))

The router now emits two histogram metrics to track memory allocation activity during request processing:

- `apollo.router.request.memory`: Memory activity across the full request lifecycle (including parsing, validation, query planning, and plugins)
- `apollo.router.query_planner.memory`: Memory activity for query planning work in the compute job thread pool

Each metric includes:

- `allocation.type`: `allocated`, `deallocated`, `zeroed`, or `reallocated`
- `context`: The tracking context name (for example, `router.request` or `query_planning`)

This feature is only available on Unix platforms when the `global-allocator` feature is enabled and `dhat-heap` is not enabled.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8717
