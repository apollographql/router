### Fix session count metrics ([Issue #3485](https://github.com/apollographql/router/issues/3485))

Previously, the `apollo_router_session_count_total` and `apollo_router_session_count_active` metrics were using counters that could become negative unexpectedly.

This issue has been fixed in this release, with **the metric type changed from counter to gauge**.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3787