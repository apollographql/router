### feat: add memory limit option for cooperative cancellation  ([PR #8808](https://github.com/apollographql/router/pull/8808))

Adds a `memory_limit` option to the `experimental_cooperative_cancellation` configuration that allows you to set a maximum memory allocation limit for query planning operations. When the memory limit is exceeded during query planning, the router will:

- **In enforce mode**: Cancel the query planning task and return an error to the client
- **In measure mode**: Record the cancellation outcome in metrics but allow the query planning to complete

In both modes, the query will be logged in a warn message.

The memory limit works alongside the existing `timeout` option, and whichever limit is reached first will trigger cancellation. This feature helps prevent excessive memory usage from complex queries or query planning operations that consume too much memory.

**Platform requirements**: This feature is only available on Unix platforms when the `global-allocator` feature is enabled and `dhat-heap` is not enabled (same requirements as memory tracking metrics).

**Example configuration:**

```yaml
supergraph:
  query_planning:
    experimental_cooperative_cancellation:
      enabled: true
      mode: enforce  # or "measure" to only record metrics
      memory_limit: 50mb  # Supports formats like "50mb", "1gb", "1024kb", etc.
      timeout: 5s  # Optional: can be combined with memory_limit
```

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8808
