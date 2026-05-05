### fix: ensure metrics are recorded for coprocessors that timeout ([PR #9296](https://github.com/apollographql/router/pull/9296))

This PR introduces two timing histogram macros, `f64_histogram_timer` and `f64_histogram_timer_with_unit`. Although it is deprecated, in this PR `f64_histogram_timer` is used to record the coprocessor timer because introducing a unit for our existing metrics will change the naming convention on services like Prometheus, which we need to avoid. Future metrics should only use `f64_histogram_timer_with_unit`.

This PR also replaces the method of timing coprocessor runs to also capture runs that time out. 

By [@conwuegb](https://github.com/conwuegb) and [@carodewig](https://github.com/carodewig) in [#9296](https://github.com/apollographql/router/pull/9296)
