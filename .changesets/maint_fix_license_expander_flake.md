### Fix flaky `license_expander` unit tests

License expiry unit tests use longer deadlines and paused Tokio time to reduce failures caused by scheduler delays. Production licensing behavior is unchanged.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/PULL_NUMBER
