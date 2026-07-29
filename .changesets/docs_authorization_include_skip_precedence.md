### Document that authorization directives take precedence over `@include`/`@skip`

The interaction between `@requiresScopes`/`@authenticated`/`@policy` and GraphQL's built-in `@include`/`@skip` directives was previously undocumented. The router enforces authorization requirements against the operation as written, so referencing a protected field produces an `UNAUTHORIZED_FIELD_OR_TYPE` error even when `@include(if: false)` or `@skip(if: true)` excludes that field from the response. The authorization documentation now states this precedence explicitly, notes that it applies to both literal and variable `if` arguments, and recommends omitting a field entirely rather than excluding it conditionally when a request doesn't meet its requirements.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9891
