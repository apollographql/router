### Add timeouts and connection health checks to Redis connections ([Issue #6855](https://github.com/apollographql/router/issues/6855))

The Router's internal Redis configuration has been improved to increase client resiliency under various failure modes (TCP failures and timeouts, unresponsive sockets, Redis server failures, etc.). It also adds heartbeats (a PING every 10 seconds) to the Redis clients.

By [@aembke](https://github.com/aembke), [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7526
