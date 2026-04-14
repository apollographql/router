### Report errors when entity fetches are skipped due to missing non-nullable required fields ([PR #9191](https://github.com/apollographql/router/pull/9191))

This fixes two bugs related to `@key`/`@requires` fields validation:

1. **Silent skipping of entity fetches**: When a non-nullable field required by `@key`/`@requires` was missing from the entity representation, the router silently skipped the downstream entity fetch, producing null fields with no errors in the response. Clients had no way to know why fields were null. The router now detects unsatisfied requires conditions per entity and generates `UNSATISFIED_FETCH_CONDITION` errors for each field that could not be fetched. This works correctly with entity batching — entities with satisfied requirements are still fetched, while only the failed entities produce errors.

2. **Incorrect fetch with unsatisfied nested required fields**: When a non-nullable field was missing inside a nested required selection (e.g., `@requires(fields: "data { a b }")` where `b` is missing), the entity representation was still sent to the subgraph with incomplete data. The router now correctly detects missing nested non-nullable fields and skips the entity fetch.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9191
