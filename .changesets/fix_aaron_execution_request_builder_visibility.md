### Restore plugin access to `SubscriptionTaskParams` in `execution::Request` builders ([PR #8771](https://github.com/apollographql/router/pull/8771))

Plugins and other external crates can use `SubscriptionTaskParams` with `execution::Request` builders again. This restores compatibility for plugin unit tests that construct subscription requests.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8771
