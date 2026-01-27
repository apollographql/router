### Add memory limit option for cooperative cancellation ([PR #8808](https://github.com/apollographql/router/pull/8808))

The router now supports a `memory_limit` option on `experimental_cooperative_cancellation` to cap memory allocations during query planning. When the memory limit is exceeded, the router:

- In `enforce` mode, cancels query planning and returns an error to the client.
- In `measure` mode, records the cancellation outcome in metrics and allows query planning to complete.

The memory limit works alongside the existing `timeout` option. Whichever limit is reached first triggers cancellation.

This feature is only available on Unix platforms when the `global-allocator` feature is enabled and `dhat-heap` is not enabled.

Example configuration:

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
