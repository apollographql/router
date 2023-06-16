### fix(deps): update rust crate router-bridge to 0.2.8+v2.4.8 ([PR #3202](https://github.com/apollographql/router/pull/3202))

[![Mend Renovate](https://app.renovatebot.com/images/banner.svg)](https://renovatebot.com)

This release bumps the Router's Federation support from v2.4.7 to v2.4.8, which brings in notable query planner fixes from [v2.4.8](https://github.com/apollographql/federation/releases/tag/@apollo/query-planner@2.4.8).  Of note from those releases, this brings query planner fixes that (per that dependency's changelog):

- Fix bug in the handling of dependencies of subgraph fetches. This bug was manifesting itself as an assertion error ([#2622](https://github.com/apollographql/federation/pull/2622))
thrown during query planning with a message of the form Root groups X should have no remaining groups unhandled (...).

- Fix issues in code to reuse named fragments. One of the fixed issue would manifest as an assertion error with a message ([#2619](https://github.com/apollographql/federation/pull/2619))
looking like Cannot add fragment of condition X (...) to parent type Y (...). Another would manifest itself by
generating an invalid subgraph fetch where a field conflicts with another version of that field that is in a reused
named fragment.

This PR contains the following updates:

| Package | Type | Update | Change |
|---|---|---|---|
| [router-bridge](https://www.apollographql.com/apollo-federation/) ([source](https://togithub.com/apollographql/federation)) | dependencies | patch | `0.2.7+v2.4.7` -> `0.2.8+v2.4.8` |

By [@renovate](https://github.com/renovate) and [o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/3202
