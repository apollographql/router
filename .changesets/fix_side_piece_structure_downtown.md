### Federation v2.4.8 ([Issue #3217](https://github.com/apollographql/router/issues/3217), [Issue #3227](https://github.com/apollographql/router/issues/3227))

This release bumps the Router's Federation support from v2.4.7 to v2.4.8, which brings in notable query planner fixes from [v2.4.8](https://github.com/apollographql/federation/releases/tag/@apollo/query-planner@2.4.8).  Of note from those releases, this brings query planner fixes that (per that dependency's changelog):

- Fix bug in the handling of dependencies of subgraph fetches. This bug was manifesting itself as an assertion error ([apollographql/federation#2622](https://github.com/apollographql/federation/pull/2622))
thrown during query planning with a message of the form `Root groups X should have no remaining groups unhandled (...)`.

- Fix issues in code to reuse named fragments. One of the fixed issue would manifest as an assertion error with a message ([apollographql/federation#2619](https://github.com/apollographql/federation/pull/2619))
looking like `Cannot add fragment of condition X (...) to parent type Y (...)`. Another would manifest itself by
generating an invalid subgraph fetch where a field conflicts with another version of that field that is in a reused
named fragment.

These manifested as Router issues https://github.com/apollographql/router/issues/3217 and https://github.com/apollographql/router/issues/3227.

By [@renovate](https://github.com/renovate) and [o0ignition0o](https://github.com/o0ignition0o) in https://github.com/apollographql/router/pull/3202
