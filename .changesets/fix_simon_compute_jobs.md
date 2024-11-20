### Move heavy computation to a thread pool with a priority queue 

These components can take non-trivial amounts of CPU time:

* GraphQL parsing
* GraphQL validation
* Query planning
* Schema introspection

In order to avoid blocking threads that execute asynchronous code,
they are now run (in their respective Rust implementations)
in a new thread pool whose size is based on available CPU cores,
with a priority queue.
Previously we used Tokio’s [`spawn_blocking`] for this purpose,
but it is appears to be intended for blocking I/O
and uses up to 512 threads so it isn’t a great fit for computation tasks.

`apollo.router.compute_jobs.queued` is a new gauge metric for the number of items in this new queue.
When the new query planner is enabled, the dedicated queue is no longer used
and the `apollo.router.query_planning.queued` metric is no longer emitted.

[`spawn_blocking`]: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6247
