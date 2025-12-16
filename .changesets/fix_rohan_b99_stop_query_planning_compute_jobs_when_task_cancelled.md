### fix: stop query planning compute jobs when parent task cancelled ([PR #8741](https://github.com/apollographql/router/pull/8741))

Ensure query planning compute jobs are stopped if the parent task is stopped by cooperative cancellation.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/8741
