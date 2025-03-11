### fix: export gauge instruments ([Issue #6859](https://github.com/apollographql/router/issues/6859))

In router 2.x, you can use the router's OTel `meter_provider()` to report metrics from Rust plugins.

Gauge instruments, such as those created using `.u64_gauge()`, were previously not exported. Now they are.

By [@yanns](https://github.com/yanns) in https://github.com/apollographql/router/pull/6865