### Federation v2.4.7 ([Issue #3170](https://github.com/apollographql/router/issues/3170), [Issue #3133](https://github.com/apollographql/router/issues/3133))

This release bumps the Router's Federation support from v2.4.7 to v2.4.7, which brings in notable query planner fixes from [v2.4.7](https://github.com/apollographql/federation/releases/tag/%40apollo%2Fquery-planner%402.4.7).  Of note from those releases, this brings query planner fixes that (per that dependency's changelog):

- Re-work the code use to try to reuse query named fragments to improve performance (thus sometimes improving query ([#2604](https://github.com/apollographql/federation/pull/2604)) planning performance)
- Fix a raised assertion error (again, with a message of form like `Cannot add selection of field X to selection set of parent type Y`).
- Fix a rare issue where an `interface` or `union` field was not being queried for all the types it should be.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3185
