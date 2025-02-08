### Remove functions related to apollo_request_processing_duration ([PR #6646](https://github.com/apollographql/router/pull/6687))

Request processing duration is already included as part of the spans sent by the
router, and there is no need for rust plugin, coprocessor and rhai plugin users
to track this information manually. As a result, the following methods and structs were removed from `context::Context`:

- `context::Context::busy_time()`
- `context::Context::enter_active_request()`
- `context::BusyTimer` struct
- `context::BusyTimerGuard` struct

By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6687