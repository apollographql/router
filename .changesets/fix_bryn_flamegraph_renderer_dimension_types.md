### Fix flamegraph rendering in diagnostics memory profiler ([PR #9837](https://github.com/apollographql/router/pull/9837))

The memory profiler's flamegraph visualization could fail to render function names. Without explicit dimension types on the ECharts custom series, ECharts inferred the `name` and `percentage` dimensions as numeric since they weren't referenced in `encode`, causing the renderer to cast function names to `NaN`.

This is now fixed by explicitly declaring the dimension types so `name` is kept as a string.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9837
