### Distributed caching: Don't send Redis' `CLIENT SETNAME`

We won't send [the `CLIENT SETNAME` command](https://redis.io/commands/client-setname/) to connected Redis servers.  This resolves an incompatibility with some Redis-compatible servers since not all "Redis-compatible" offerings (like Google Memorystore) actually support _every_ Redis command.  We weren't actually necessitating this feature, it was just a feature that could be enabled optionally on our Redis client.  No Router functionality is impacted.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2825