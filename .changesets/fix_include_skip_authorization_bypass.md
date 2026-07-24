### Don't flag statically-excluded fields as unauthorized ([PR #9859](https://github.com/apollographql/router/pull/9859))

A field carrying a literal `@include(if: false)` or `@skip(if: true)` is never sent to a subgraph, but `@requiresScopes`, `@authenticated`, and `@policy` were still evaluated against it, producing an `UNAUTHORIZED_FIELD_OR_TYPE` error even when the field could never have been resolved. The router now skips the authorization check for a field or fragment excluded this way, and that exemption propagates to any selections nested under it.

A variable-conditioned `@include`/`@skip` (for example `@include(if: $showDetails)`) is unaffected by this change: the query planner doesn't evaluate the variable either, so the field may still be sent to a subgraph depending on its runtime value, and is still checked as before.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9859
