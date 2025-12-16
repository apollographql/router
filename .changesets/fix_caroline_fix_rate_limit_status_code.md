### Raise 429 rather than 503 when enforcing rate-limit ([PR #8765](https://github.com/apollographql/router/pull/8765))

In the release of router 2.0, the rate-limiting error raised was changed from 429 (`TOO_MANY_REQUESTS`) to 503 (
`SERVICE_UNAVAILABLE`). This change reverts that modification to align with the
router [documentation](https://www.apollographql.com/docs/graphos/routing/errors#429).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/8765
