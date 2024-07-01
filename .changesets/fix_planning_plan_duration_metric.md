### Improve accuracy of `query_planning.plan.duration` ([PR #5](https://github.com/apollographql/router/pull/5530))
Previously, the `apollo.router.query_planning.plan.duration` metric inaccurately included additional processing time beyond query planning. The additional time included pooling time, which is already accounted for in the metric. After this update, apollo.router.query_planning.plan.duration now accurately reflects only the query planning duration without additional processing time.

For example, before the change, metrics reported: 
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