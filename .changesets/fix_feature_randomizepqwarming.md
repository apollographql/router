### Update randomization of query prewarming order to also apply to persisted queries ([PR #6528](https://github.com/apollographql/router/pull/6528))

Update randomization of query prewarming order to also apply to persisted queries. When using distributed caching, this allows deduplication of effort in a Router fleet since each Router will do pre-warming in a different order and check against the Redis cache if a particular query has already been planned by another Router.

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6528
