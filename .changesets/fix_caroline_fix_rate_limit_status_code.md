### Return `429` instead of `503` when enforcing a rate limit ([PR #8765](https://github.com/apollographql/router/pull/8765))

In v2.0.0, the router changed the rate-limiting error from `429` (`TOO_MANY_REQUESTS`) to `503` (`SERVICE_UNAVAILABLE`). This change restores `429` to align with the [router error documentation](https://www.apollographql.com/docs/graphos/routing/errors#429).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8765
