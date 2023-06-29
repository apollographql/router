### Fix @defer inside an aliased field ([Issue #3263](https://github.com/apollographql/router/issues/3263))

This refactors [PR #3298](https://github.com/apollographql/router/pull/3298/) to prepare deferred subselections without serializing them to GraphQL syntax just to re-parse them. This removes code that did not correctly handle some interactions of `@defer` with aliases.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3346
