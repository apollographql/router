### Add compute pool metrics ([PR #7184](https://github.com/apollographql/router/pull/7184))

The compute job pool is used within the router for compute intensive jobs that should not block the Tokio worker threads.
When this pool becomes saturated it is difficult for users to see why so that they can take action.
This change adds new metrics to help users understand how long jobs are waiting to be processed.  

New metrics:
- `apollo.router.compute_jobs.queue_is_full` - A counter of requests rejected because the queue was full.
- `apollo.router.compute_jobs.duration` - A histogram of time spent in the compute pipeline by the job, including the queue and query planning.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)
  - `job.outcome`: (`executed_ok`, `executed_error`, `channel_error`, `rejected_queue_full`, `abandoned`)
- `apollo.router.compute_jobs.queue.wait.duration` - A histogram of time spent in the compute queue by the job.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)
- `apollo.router.compute_jobs.execution.duration` - A histogram of time spent to execute job (excludes time spent in the queue).
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)
- `apollo.router.compute_jobs.active_jobs` - A gauge of the number of compute jobs being processed in parallel.
  - `job.type`: (`query_planning`, `query_parsing`, `introspection`)

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7184
