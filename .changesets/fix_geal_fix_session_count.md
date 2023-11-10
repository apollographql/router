### fix the session count metric ([Issue #3485](https://github.com/apollographql/router/issues/3485))

The `apollo_router_session_count_total` and `apollo_router_session_count_active` metrics were using counters which were flushed and reset from time to time, which meant that sometimes the value could become negative. Now the router stores the actual session count separately.
**To support this, the metric type has been changed from counter to gauge**

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3787