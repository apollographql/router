### Fix graceful shutdown with in flight client requests ([Issue #2539](https://github.com/apollographql/router/issues/2539))

Wait for all in flight client requests to have finished before shutting down the router. Previously, we implemented graceful shutdown, but only for configuration reloiads. This takes care of the shutdown phase.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2610