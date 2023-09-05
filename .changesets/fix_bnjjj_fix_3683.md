### fix(subscription): add x-accel-buffering header for multipart response ([Issue #3683](https://github.com/apollographql/router/issues/3683))

Set `x-accel-buffering` to `no` when it's a multipart response because proxies need this configuration.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3749
