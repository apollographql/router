### Report errors when entity fetches are skipped due to missing non-nullable required fields ([PR #9191](https://github.com/apollographql/router/pull/9191))

When a non-nullable field required by `@key`/`@requires` is missing from the entity representation, the router used to silently skip the downstream entity fetch — the response carried null fields with no errors and clients had no way to tell why. The router now detects these unsatisfied conditions per entity and generates `UNSATISFIED_FETCH_CONDITION` errors at the affected paths. Entity batching is handled correctly: entities with satisfied requirements are still fetched, and only the failed entities produce errors.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9191
