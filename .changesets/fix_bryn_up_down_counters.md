### Prevent UpDownCounter drift using RAII guards ([PR #8379](https://github.com/apollographql/router/pull/8379))

UpDownCounters now use RAII guards instead of manual incrementing and decrementing, ensuring they're always decremented when dropped.

This fix resolves drift in `apollo.router.opened.subscriptions` that occurred due to manual incrementing and decrementing.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8379
