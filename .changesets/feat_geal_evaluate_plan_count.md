### Add a histogram metric tracking evaluated query plans ([PR #5875](https://github.com/apollographql/router/pull/5875))

The router supports the new `apollo.router.query_planning.plan.evaluated_plans` histogram metric to track the number of evaluated query plans. 

You can use it to help set an optimal `supergraph.query_planning.experimental_plans_limit` option that limits the number of query plans evaluated for a query and reduces the time spent planning.


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5875