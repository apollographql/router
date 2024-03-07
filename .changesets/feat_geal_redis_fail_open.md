### Redis: add a fail open option ([Issue #4334](https://github.com/apollographql/router/issues/4334))

This option configures the Router's behavior in case it cannot connect to Redis:
- by default, it will still start, so requests will still be handled in a degraded state
- when active, that option will prevent the router from starting if it cannot connect

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4534