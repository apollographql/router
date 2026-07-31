### Keep Redis replica connections alive to prevent a connection-churn loop ([PR #9913](https://github.com/apollographql/router/pull/9913))

On Redis **cluster** deployments using `response_cache` or entity caching, the router opens a connection to every replica eagerly. Those replica connections received no keep-alive traffic (the built-in heartbeat only pings the primary), so a server-side idle-connection timeout — for example AWS ElastiCache, or an NLB that reaps idle sockets — could close them. Each idle close triggered an eager replica re-sync that reopened all replica connections at once, which then went idle together and were reaped again: a self-sustaining reconnect loop that could run even with no request traffic, collapsing the cache hit ratio to near zero.

The router now sends periodic keep-alive `PING`s to replica connections on the same interval as the primary heartbeat, so idle replica sockets are no longer reaped and the loop can't start.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9913
