### fix: ensure metrics are recorded for coprocessors that timeout ([PR #9296](https://github.com/apollographql/router/pull/9296))

`apollo.router.operations.coprocessor.duration` is now recorded even when a coprocessor call is cut short by a router timeout. Previously, the metric was only emitted when the call completed normally.

This fix also introduces the `f64_histogram_timer_with_unit!` macro for use in future metrics. It returns a guard that automatically records seconds elapsed when the guard is dropped.

By [@conwuegb](https://github.com/conwuegb) and [@carodewig](https://github.com/carodewig) in [#9296](https://github.com/apollographql/router/pull/9296)
