### Clamp license timers to a safe limit ([PR #9561](https://github.com/apollographql/router/pull/9561))

A router started with a license whose expiry date falls more than roughly two years in the future crashed on startup with invalid deadline; err=Invalid. It now starts and serves traffic normally with such licenses.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/9561
