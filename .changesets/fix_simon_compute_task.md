### Move computation-heavy tasks to a dedicated thread pool ([PR #6122](https://github.com/apollographql/router/pull/6122))

These components can take non-trivial amounts of CPU time:
* GraphQL parsing
* GraphQL validation
* Query planning
* Schema introspection

In order to avoid blocking threads that execute asynchronous code, they are now run (in their respective Rust implementations) in a new pool of as many threads as CPU cores are available. Previously we used Tokio’s [`spawn_blocking`] for this purpose, but it is appears to be intended for blocking I/O and uses up to 512 threads so it isn’t a great fit for computation tasks.

[`spawn_blocking`]: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/6122
