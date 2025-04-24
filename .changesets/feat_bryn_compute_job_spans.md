### Compute job spans ([PR #7236](https://github.com/apollographql/router/pull/7236))

The router uses a separate thread pool called "compute jobs" to ensure that requests do not block tokio io worker threads.
This PR adds spans to jobs that are on this pool to allow users to see when latency is introduced due to 
resource contention within the compute job pool.

* `compute_job`:
  - `job.type`: (`QueryParsing`|`QueryParsing`|`Introspection`)
* `compute_job.execution`
  - `job.age`: `P1`-`P8`
  - `job.type`: (`QueryParsing`|`QueryParsing`|`Introspection`)

Jobs are executed highest priority (`P8`) first. Jobs that are low priority (`P1`) age over time, eventually executing 
at highest priority. The age of a job is can be used to diagnose if a job was waiting in the queue due to other higher 
priority jobs also in the queue.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/7236
