### Merge top-level fragments that become identical after removing redundant `@include`/`@skip` ([PR #10376](https://github.com/apollographql/router/pull/10376))

When building fetches, the query planner removes `@include`/`@skip` from top-level inline fragments when the condition is already implied by where the fetch is attached. Two fragments that differed only by such a directive could then end up with the same selection key but be stored as separate entries, breaking an invariant that selection-set lookups and merges rely on. They are now merged into one fragment. This is a defensive fix: no current query plan was found to hit it.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10376
