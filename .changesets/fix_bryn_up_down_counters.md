### (refactor) UpDownCounter RAII guards ([PR #8379](https://github.com/apollographql/router/pull/8379))

Previously UpDownCounters were being manually incremented and decremented. This PR changes UpDownCounters to use RAII guards
on drop ensuring that they are always decremented when dropped.

In particular this fixes: `apollo.router.opened.subscriptions` which was previously drifting due to manual incrementing and decrementing.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8379
