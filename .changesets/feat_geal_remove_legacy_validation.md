### Remove legacy validation ([PR #4551](https://github.com/apollographql/router/pull/4551))

GraphQL query validation was initially performed by the query planner in JavaScript, which caused some performance issues. Here, we are introducing a new Rust-based validation process using `apollo-compiler` from the `apollo-rs` project. This validation is also happening much earlier in the process, inside the "router service" instead of the query planner, which will reduce the load on the query planner and give back some room in the query planner cache.

Because validation now happens early, some error paths deeper inside the router will no longer be hit, causing observable differences in error messages. The new messages should be clearer and more useful.

This new validation process has been running in production for months concurrently with the JavaScript version, allowing us to detect and fix any discrepancies in the new implementation. We now have enough confidence in the new Rust-based validation to entirely switch off the less performant, JavaScript validation.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4551
