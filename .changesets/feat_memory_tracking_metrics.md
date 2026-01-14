### Implement memory tracking metrics for requests ([PR #8717](https://github.com/apollographql/router/pull/8717))

Adds two histogram metrics to track memory allocation patterns during request processing:

- `apollo.router.request.memory` - Tracks memory allocations for the entire request lifecycle, including GraphQL parsing, validation, query planning, response composition, and plugin execution
- `apollo.router.query_planner.memory` - Tracks memory allocations specifically for query planning operations executed in the compute job thread pool

Both metrics record four types of memory operations (allocated, deallocated, zeroed, reallocated) with histogram buckets at 1KB, 10KB, 100KB, 1MB, 10MB, and 100MB. Each metric includes attributes:
- `allocation.type`: The type of memory operation (`allocated`, `deallocated`, `zeroed`, `reallocated`)
- `context`: The context name where the allocation occurred (e.g., `router.request`, `query_planning`)

The implementation uses a custom allocator wrapper over jemalloc that tracks allocations via thread-local storage with task-local propagation, supporting nested tracking contexts. This feature is only available on Unix platforms when the `global-allocator` feature is enabled and `dhat-heap` is not enabled.


By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8717
