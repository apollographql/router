## 🐛 Fixes

### Federation v2.4.5

This release bumps the Router's Federation support from v2.4.2 to v2.4.5, which brings in notable query planner fixes from [v2.4.3](https://github.com/apollographql/federation/releases/tag/%40apollo%2Fquery-planner%402.4.2) and [v2.4.5](https://github.com/apollographql/federation/releases/tag/%40apollo%2Fquery-planner%402.4.5).  **Federation v2.4.4 will not exist** due to a publishing failure.  Of note from those releases, this brings query planner fixes that:

- Improves the heuristics used to try to reuse the query named fragments in subgraph fetches. Said fragment will be reused ([apollographql/federation#2541](https://github.com/apollographql/federation/pull/2541)) more often, which can lead to smaller subgraph queries (and hence overall faster processing).
- Fix potential assertion error during query planning in some multi-field `@requires` case. This error could be triggered ([#2575](https://github.com/apollographql/federation/pull/2575)) when a field in a `@requires` depended on another field that was also part of that same requires (for instance, if a field has a `@requires(fields: "id otherField")` and that `id` is also a key necessary to reach the subgraph providing `otherField`).
  
  The assertion error thrown in that case contained the message `Root groups (...) should have no remaining groups unhandled (...)`

By [@abernix](https://github.com/abernix) in https://github.com/apollographql/router/pull/3107
