### Improved demand control cost estimation by considering Boolean conditions and type conditions ([PR #9209](https://github.com/apollographql/router/pull/9209))

The static cost calculator now evaluates `@skip` / `@include` conditions against request variables, picks the correct branch of `PlanNode::Condition` based on its condition variable, and avoids summing costs from mutually-exclusive fragment spreads.

- `@skip(if: $var)` and `@include(if: $var)` directives are now resolved against the supplied variables for fields, fragment spreads, and inline fragments. Previously only boolean literals were handled, so variable-driven inclusion always treated the selection as present.
- `PlanNode::Condition` (the node the query planner emits for `@defer(if: $var)` and similar conditionals) now picks the branch that will actually run at request time instead of taking the maximum cost of both branches.


Selection set cost estimator now group selections by their statically known narrowest type. Selections that apply to abstract types form a shared "abstract" bucket whose cost is always added. Each observed concrete object type condition gets its own bucket, and because only one concrete type can apply at runtime the estimator takes the MAX across concrete buckets rather than summing them.

- Nested spreads carry the outer narrowing through, so `... on Cat { ... on Pet { x } }` still contributes `x` to the `Cat` bucket.
- Within each bucket, occurrences sharing a response key collapse via MAX per key to honor GraphQL field merging. This prevents spreads across many implementation types (e.g. `... on A { a } ... on B { b } ... on C { c }`) from inflating the estimate by summing every mutually-exclusive branch.

As a result, estimated costs now more closely track the real shape of the request and no longer over-count fields that will be skipped by the runtime, merged by field merging, or ruled out by the runtime type.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/9209