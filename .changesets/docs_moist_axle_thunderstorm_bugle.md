### [docs] Add new page for query planning best practices ([PR #7263](https://github.com/apollographql/router/pull/7263))

We had an existing page that contained a short subsection on query planner impact. This really needs to be it's own page that can be expanded upon in the future.

To start I copied the [existing page](https://www.apollographql.com/docs/graphos/platform/production-readiness/deployment-best-practices#changes-affecting-query-planner-performance) but added more details.

I think we can come back and clean up the old page which specifically is talking about the order of API schema changes in CI/CD for these scenarios which is different from the query planner runtime

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/7263
