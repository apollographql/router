### Ensure query planning allocation stats are still recorded if cooperative cancellation is not enabled ([PR #8902](https://github.com/apollographql/router/pull/8902))

The metric `apollo.router.query_planner.memory` was unintentionally only showing allocations during the `query_parsing` compute job if cooperative cancellation for query planning was not enabled. Both `query_parsing` and `query_planning` should now be available.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8902
