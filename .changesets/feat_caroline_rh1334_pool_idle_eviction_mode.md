### Fix connection pool to use lazy idle eviction ([PR #9308](https://github.com/apollographql/router/pull/9308))

When `pool_idle_timeout` was introduced in v2.13.0, the router unconditionally enabled a background timer that proactively closed idle connections exceeding the timeout. In some network environments, the TCP close sent by this background task raced with a new connection attempt and caused significant latency spikes on the next request.

The router now uses lazy eviction: connections are only closed at checkout time, when a request finds a pooled connection that has exceeded `pool_idle_timeout`. No TCP closes are sent between requests. This matches router behavior before v2.13.0.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9308
