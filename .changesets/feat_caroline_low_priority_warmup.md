### De-prioritize warm-up process query parsing and planning ([PR #7223](https://github.com/apollographql/router/pull/7223))

The router warms up its query planning cache after a schema or configuration change. This change decreases the priority
of warm up tasks in the compute job queue, to reduce the impact of warmup on serving requests.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7223
