### Update all metrics to use `.` naming convention ([PR #6653](https://github.com/apollographql/router/pull/6653))

Some of the older metrics in the router were not using the most recent `.` naming convention and were instead separated by `_`. For consistency purposes, the following metrics are renamed:

| Previous metric | Renamed metric |
| --------------- | -------------- |
| `apollo_router_opened_subscriptions` | `apollo.router.opened.subscriptions` |
| `apollo_router_cache_hit_time` | `apollo.router.cache.hit.time` |
| `apollo_router_cache_size` | `apollo.router.cache.size` |
| `apollo_router_cache_hit_count` | `apollo.router.cache.hit.count` |
| `apollo_router_cache_miss_time` | `apollo.router.cache.miss.time` |
| `apollo_router_cache_miss_count` | `apollo.router.cache.miss.count` |
| `apollo_router_state_change_total` | `apollo.router.state.change.total` |
| `apollo_router_span_lru_size` | `apollo.router.exporter.span.lru.size` $_{[1]} |
| `apollo_router_session_count_active` | `apollo.router.session.count.active` |
| `apollo_router_uplink_fetch_count_total` | `apollo.router.uplink.fetch.count.total` |
| `apollo_router_uplink_fetch_duration_seconds` | `apollo.router.uplink.fetch.duration.seconds`|

$_{[1]} `apollo.router.exporter.span.lru.size` now also has an additional `exporter` prefix.


By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/6653
