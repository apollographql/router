### fix(metrics) make query_planning.plan.duration more accurate ([PR #5](https://github.com/apollographql/router/pull/5530))

`apollo.router.query_planning.plan.duration` was previously recording additional
processing duration aside from just query planning duration data. This was an
inaccurate representation of the actual query planning time, and is now fixed.
The additional processing data included pooling time, which is already
represented as part of `apollo.router.query_planning.total.duration` metric.

Here is an example of what `apollo.router.query_planning.plan.duration` and
`apollo.router.query_planning.total.duration` would be reporting before this
change:
```bash
2024-06-21T13:37:27.744592Z WARN  apollo.router.query_planning.plan.duration 0.002475708
2024-06-21T13:37:27.744651Z WARN  apollo.router.query_planning.total.duration 0.002553958

2024-06-21T13:37:27.748831Z WARN  apollo.router.query_planning.plan.duration 0.001635833
2024-06-21T13:37:27.748860Z WARN  apollo.router.query_planning.total.duration 0.001677167
```
And here is what this is reporting after this change:
```bash
2024-06-21T13:37:27.743465Z WARN  apollo.router.query_planning.plan.duration 0.00107725
2024-06-21T13:37:27.744651Z WARN  apollo.router.query_planning.total.duration 0.002553958

2024-06-21T13:37:27.748299Z WARN  apollo.router.query_planning.plan.duration 0.000827
2024-06-21T13:37:27.748860Z WARN  apollo.router.query_planning.total.duration 0.001677167
```

By [@xuorig](https://github.com/xuorig) and [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/5530