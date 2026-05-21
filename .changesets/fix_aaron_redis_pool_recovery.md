### Recover the Redis-backed caches after cluster events and honor `required_to_start: true` on startup

The router's Redis-backed caches (query planner, entity cache, APQ, response cache) could silently stall after a network event involving Redis replicas or the full cluster — accumulating queued commands, command timeouts, latency, and memory pressure until the router was redeployed.  The router now detects when the underlying Redis client has given up reconnecting, drains the connection pool, and rebuilds it on the next request.  In deployments where the broadcast cluster topology contains nodes that aren't routeable from the router's network position (for example, internal IPs reserved for replica promotion), a new replica filter screens those nodes out before they enter the routing table.

The `required_to_start: true` flag — available on each cache under `supergraph.query_planning.cache.redis`, `apq.router.cache.redis`, `preview_entity_cache.subgraph.all.redis`, and `experimental_response_cache.subgraph.all.redis` — now actually fails the router's startup fast when Redis is unreachable, instead of hanging indefinitely or silently returning success under broadcast overflow.

The router also now supports `required_to_start: false`, allowing the router to start when Redis is unavailable at boot and to begin caching once Redis becomes reachable.

For more technical internal details, see [PR #9023](https://github.com/apollographql/router/pull/9023) and [PR #9418](https://github.com/apollographql/router/pull/9418).  For more details on configuring the router's Redis-backed caches, see [Response Cache Customization](https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/customization) and the related caching docs.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9023 and https://github.com/apollographql/router/pull/9418
