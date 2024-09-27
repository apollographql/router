### Add metrics for Rust vs. Deno configuration values ([PR #6056](https://github.com/apollographql/router/pull/6056))

We are working on migrating the implementation of several JavaScript components in the router to native Rust versions.

To track this work, the router now reports the values of the following configuration options to Apollo:
- `apollo.router.config.experimental_query_planner_mode`
- `apollo.router.config.experimental_introspection_mode`

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/6056