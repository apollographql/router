### Query plan JSON gives fetches an explicit kind and flatten nodes a list of members ([PR #10841](https://github.com/apollographql/router/pull/10841))

The JSON query plan format changed, including the plan returned in `extensions.apolloQueryPlan.object`:

- A fetch's `fetchKind.kind` is `Root` or `Entities`. An entity fetch's `requires`, `inputRewrites`, and `contextRewrites` move inside `fetchKind`.
- A `Flatten` node holds `members` instead of `path` and `node`. Each member has `paths`, a list of response paths, and a `fetch` without a `kind: "Fetch"` tag. A v2 flatten's `node` and `path` map to `members[0].fetch` and `members[0].paths[0]`.

Query plans and response data are otherwise unchanged. Update any tooling that parses exposed query plans.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/10841
