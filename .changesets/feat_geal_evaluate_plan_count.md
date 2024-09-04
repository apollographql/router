### Add a histogram metric tracking evaluated query plans ([PR #5875](https://github.com/apollographql/router/pull/5875))

The `supergraph.query_planning.experimental_plans_limit` option can be used to limit the number of query plans evaluated for a query, to reduce the time spent planning. When reaching that limit, the planner would still return a valid query plan, but maybe the most optimized one.
This adds the `apollo.router.query_planning.plan.evaluated_plans` histogram metric to track the number of evaluated query plans, giving more context to configure this option.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5875