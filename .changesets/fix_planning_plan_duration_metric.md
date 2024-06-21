### fix(metrics) make query_planning.plan.duration more accurate ([PR #5428](https://github.com/apollographql/router/pull/5428))

`apollo.router.query_planning.plan.duration` was previously recording identical
data to `apollo.router.query_planning.total.duration` metric, incuding queue and
pool time. This was an inaccurate representation of the actual query planning
time, and is now fixed.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/5428