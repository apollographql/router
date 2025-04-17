### De-prioritize warm-up process query parsing and planning ([PR #7223](https://github.com/apollographql/router/pull/7223))

The router warms up its query planning cache after a schema or configuration change. This change decreases the priority
of warm up tasks in the compute job queue, to reduce the impact of warmup on serving requests.

This change adds new values to the `job.type` dimension of the following metrics:
- `apollo.router.compute_jobs.duration` - A histogram of time spent in the compute pipeline by the job, including the queue and query planning.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)
  - `job.outcome`: (`executed_ok`, `executed_error`, `channel_error`, `rejected_queue_full`, `abandoned`)
- `apollo.router.compute_jobs.queue.wait.duration` - A histogram of time spent in the compute queue by the job.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)
- `apollo.router.compute_jobs.execution.duration` - A histogram of time spent to execute job (excludes time spent in the queue).
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)
- `apollo.router.compute_jobs.active_jobs` - A gauge of the number of compute jobs being processed in parallel.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`, **`query_planning_warmup`, `query_parsing_warmup`**)


By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7223
