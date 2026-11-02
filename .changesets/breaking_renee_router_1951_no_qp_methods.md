### Remove `experimental_reuse_query_plans` ([PR #9772](https://github.com/apollographql/router/pull/9772))

The experimental query plan reuse feature has been removed. This has been experimental for several years and never proved useful enough to stabilize. We recommend using a distributed query plan cache to get most of the benefit.

By [@goto-bus-stop](https://github.com/goto-bus-stop) in https://github.com/apollographql/router/pull/9772