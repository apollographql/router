### jemalloc metrics ([PR #7735](https://github.com/apollographql/router/pull/7735))

The router adds the following new metrics when running the router on Linux with its default `global-allocator` feature:

- [`apollo_router_jemalloc_active`](https://jemalloc.net/jemalloc.3.html#stats.active): Total number of bytes in active pages allocated by the application.
- [`apollo_router_jemalloc_allocated`](https://jemalloc.net/jemalloc.3.html#stats.allocated): Total number of bytes allocated by the application.
- [`apollo_router_jemalloc_mapped`](https://jemalloc.net/jemalloc.3.html#stats.mapped): Total number of bytes in active extents mapped by the allocator.
- [`apollo_router_jemalloc_metadata`](https://jemalloc.net/jemalloc.3.html#stats.metadata): Total number of bytes dedicated to metadata, which comprise base allocations used for bootstrap-sensitive allocator metadata structures and internal allocations.
- [`apollo_router_jemalloc_resident`](https://jemalloc.net/jemalloc.3.html#stats.resident): Maximum number of bytes in physically resident data pages mapped by the allocator, comprising all pages dedicated to allocator metadata, pages backing active allocations, and unused dirty pages.
- [`apollo_router_jemalloc_retained`](https://jemalloc.net/jemalloc.3.html#stats.retained): Total number of bytes in virtual memory mappings that were retained rather than being returned to the operating system via e.g. `munmap(2)` or similar.

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7735
