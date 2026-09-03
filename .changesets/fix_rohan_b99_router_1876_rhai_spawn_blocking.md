### Run Rhai script hooks on the blocking thread pool ([PR #9815](https://github.com/apollographql/router/pull/9815))

Rhai request and response hooks were evaluated inline on the Tokio async executor thread. A script with a loop, regex, or other CPU-bound work held that thread for the duration of its evaluation, delaying every other in-flight request scheduled on it.

Rhai script evaluation now runs via `tokio::task::spawn_blocking`, so it executes on Tokio's dedicated blocking thread pool instead of an async executor thread.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9815
