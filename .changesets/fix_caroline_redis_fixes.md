### Respect Redis cluster slots when inserting multiple items ([PR #8185](https://github.com/apollographql/router/pull/8185))

The existing `insert` code will silently fail when we try to insert multiple values which correspond to different [Redis cluster hash slots](https://redis.io/docs/latest/operate/oss_and_stack/reference/cluster-spec/#key-distribution-model). This PR corrects that behavior, raises errors when inserts fail, and adds new metrics to track Redis client health.

New metrics:
* `apollo.router.cache.redis.unresponsive`: counter for 'unresponsive' events raised by the Redis library
  * `kind`: Redis cache purpose (`APQ`, `query planner`, `entity`)
  * `server`: Redis server that became unresponsive
* `apollo.router.cache.redis.reconnection`: counter for 'reconnect' events raised by the Redis library
  * `kind`: Redis cache purpose (`APQ`, `query planner`, `entity`)
  * `server`: Redis server that required client reconnection

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8185