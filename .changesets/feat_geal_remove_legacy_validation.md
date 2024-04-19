### Query validation process with Rust ([PR #4551](https://github.com/apollographql/router/pull/4551))

The router has been updated with a new Rust-based query validation process using `apollo-compiler` from the `apollo-rs` project. It replaces the Javascript implementation in the query planner. It improves query planner performance by moving the validation out of the query planner and into the router service, which frees up space in the query planner cache. 

Because validation now happens earlier in the router service and not in the query planner, error paths in the query planner are no longer encountered. The new error messages should be clearer.

We've tested the new validation process by running it for months in production, concurrently with the JavaScript implementation, and have now completely transitioned to the Rust-based implementation.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4551
