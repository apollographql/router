### Fix connector composition failures for chained `->filter`/`->find` selections

A `connect/v0.4` connector selection that places one of the iterative array methods after another array-producing method — for example `$.items->filter(@.a->eq("x"))->filter(@.b->gt(0)) { id name }` or `$.items->filter(@.a->eq("x"))->find(@.b->gt(0)) { id name }` — could fail to compose with a `SATISFIABILITY_ERROR`.

`->filter` and `->find` shaped their condition against the whole input array instead of its element. A single method applied to an `Unknown` input happened to pass a lenient boolean check, but once a prior method handed it a concrete `List<…>`, the condition `@.field->…` was evaluated against the list, produced a spurious "condition must return a boolean value" error, and connector expansion collapsed the output object to a type with no fields — an invalid subgraph that surfaced downstream as a composition error.

Both methods now shape their condition against the array element (`any_item`), matching the per-element runtime semantics, so chained selections expand and compose correctly. (`->map` already shaped its callback per element and was unaffected.)

By [@benjamn](https://github.com/benjamn) in https://github.com/apollographql/router/pull/9643
