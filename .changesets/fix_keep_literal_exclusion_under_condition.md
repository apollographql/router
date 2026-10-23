### Keep constant `@include(if: false)` / `@skip(if: true)` exclusions inside conditionally included selections ([PR #10373](https://github.com/apollographql/router/pull/10373))

When a selection guarded by a variable condition (for example `obj @include(if: $x)`) contained a field excluded by a constant condition (for example `dead @include(if: false)`), the query planner dropped the constant directive while removing the variable condition it had already moved into an `Include`/`Skip` plan node. The subgraph was then asked to resolve the excluded field unconditionally. The field was still filtered out of the client response, but the subgraph did unnecessary work and could fail on a field the client never asked for.

The planner now only removes conditions that are actually handled by the enclosing plan node, and keeps constant exclusions in the subgraph operation.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10373
