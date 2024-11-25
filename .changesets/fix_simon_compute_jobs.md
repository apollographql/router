### Move heavy computation to a thread pool with a priority queue ([PR #6247](https://github.com/apollographql/router/pull/6247))

The router now avoids blocking threads when executing asynchronous code by using a thread pool with a priority queue.

This improves the performance of the following components can take non-trivial amounts of CPU time:

* GraphQL parsing
* GraphQL validation
* Query planning
* Schema introspection

In order to avoid blocking threads that execute asynchronous code,
they are now run in a new thread pool with a priority queue. The size of the thread pool is based on the number of available CPU cores.

The thread pool replaces the router's prior implementation that used Tokio’s [`spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).

`apollo.router.compute_jobs.queued` is a new gauge metric for the number of items in the thread pool's priority queue. 

> Note: when the native query planner is enabled, the dedicated queue of the legacy query planner is no longer used, so the `apollo.router.query_planning.queued` metric is no longer emitted.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6247
