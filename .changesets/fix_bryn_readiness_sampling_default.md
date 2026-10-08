### Readiness sampling interval defaults to 5s when omitted

A health check readiness `interval` block that omitted `sampling`, such as `health_check.readiness.interval.unready: 10s` or an empty `interval: {}`, gave a zero sampling interval. The readiness ticker panicked on it, so the router never reported unready however many requests it rejected. `sampling` now defaults to the documented 5s whenever it is omitted, and the configuration JSON Schema reports that default.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/PULL_NUMBER
